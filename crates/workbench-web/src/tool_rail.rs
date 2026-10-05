//! `ToolRail` — the Graph page's control surface (GRA-01–GRA-04, GRA-06, D-01–D-10): the
//! algorithm picker, all six per-algorithm parameter forms, the edge-type checklist, the canvas
//! legend, and the Run/Cancel run button wired to `POST /api/graph/algorithm`/`.../cancel`.
//!
//! Mounted by `graph_page.rs` beside the canvas (plan 44-10/44-16). The results list and status
//! line (plan 44-13) are NOT this plan's job — this module renders only the rail's control
//! surface, per this plan's own objective text.
//!
//! # Testing approach (plain-function-first, per `analysis_page.rs`'s documented convention)
//!
//! Every piece of transcription-sensitive logic — per-algorithm defaults, the D-01 forced-
//! Represents-inclusion rule, the Run-enabled gate, the out-of-scope check, the failure-copy
//! format — is factored into plain functions taking owned/borrowed values and returning
//! `bool`/`String`/`AlgorithmRequest`, fully host-testable without a DOM. `trigger_algorithm_run`/
//! `trigger_algorithm_cancel` call `gloo-net`'s Fetch-API transport directly and are not
//! unit-tested (same rationale as `analysis_page.rs::trigger_run`/`fetch_report`).
//!
//! Module-wide `#![allow(dead_code)]`: Task 1 (this file) lands before Task 2 wires `ToolRail`
//! into `graph_page.rs` in the same plan — mirrors `cosmograph_trait.rs`/`entity_picker.rs`'s own
//! documented convention for a component committed ahead of its mount site.
#![allow(dead_code)]

use leptos::prelude::*;
use urban_integrity_workbench_dto::caps;
use urban_integrity_workbench_dto::{
    AlgorithmOutcome, AlgorithmRequest, AlgorithmResult, EntityRef,
};

use crate::abbr::Abbr;
use crate::edge_palette::EDGE_TYPE_PALETTE;
use crate::entity_picker::EntityPicker;
use crate::node_palette::NODE_KIND_PALETTE;

/// `POST /api/graph/algorithm` — plan 44-09's native dispatch endpoint.
pub const ALGORITHM_URL: &str = "/api/graph/algorithm";
/// `POST /api/graph/algorithm/cancel` — plan 44-09's native cancel endpoint.
pub const ALGORITHM_CANCEL_URL: &str = "/api/graph/algorithm/cancel";

/// The glossary token the canvas legend's shared OSINT swatch label wraps in `<Abbr>` — a bare
/// `pub const`, not an inline string literal, following the exact `CLI_ABBR` pattern in
/// `crates/workbench-web/src/help/mod.rs` (the phase 44 UI spec "Canvas legend").
pub const OSINT_ABBR: &str = "OSINT";
/// The second glossary token the same legend label wraps in `<Abbr>` — same rationale as
/// [`OSINT_ABBR`].
pub const PSC_ABBR: &str = "PSC";

/// D-09's fixed pivot count, rendered as static Help text beneath the algorithm name whenever
/// betweenness is selected — this is NOT a rail input field (D-09: "the planner picks the
/// default… and records it in the rail's Help text"). The literal `500` here is rail copy, kept
/// in sync BY HAND with `crate::graph::algorithms::betweenness::DEFAULT_BETWEENNESS_PIVOT_COUNT`
/// (plan 44-05, in `urban-integrity-workbench`, not reachable from this wasm-only crate — see
/// the phase 44 research notes/this crate's Cargo.toml doc comment on the forbidden-dependency guard) — the
/// value plan 44-09's dispatch call site actually uses. If that constant ever changes, this
/// string must change by hand to match.
pub const BETWEENNESS_PIVOT_HELP: &str = "Approximate: samples 500 source nodes, seeded.";

/// The exact D-01 inline text rendered beneath the edge-type checklist's `Represents` row when
/// ward-reachable subgraph is selected (the phase 44 UI spec "Tool rail" section 3).
const WARD_REPRESENTS_FORCED_TEXT: &str =
    "Always walks Represents from the picked ward — this algorithm's edge is the ward itself.";

/// The Copywriting Contract's out-of-scope error copy (verbatim), with `{name}` substituted —
/// shown inline beneath a picker field whose resolved entity is outside the current graph
/// population (T-44-11-02: this rail owns the out-of-scope check, `EntityPicker` itself is
/// scope-agnostic).
pub fn out_of_scope_copy(name: &str) -> String {
    format!("{name} is outside the current graph population and cannot be used in an algorithm.")
}

/// [`EntityPicker`]'s placeholder text (the phase 44 UI spec "Entity picker" Empty state), shared across
/// every From/To/Ward field this rail mounts.
const PICKER_PLACEHOLDER: &str = "Name, code or id\u{2026}";

/// The eleven edge-type checklist rows, in the exact order phase 44 design decision D-01 lists them (the
/// eight default-checked types first, then the three default-unchecked hub-forming types) — the
/// single source of truth this rail's checklist iterates and [`default_edge_types_included`]'s
/// test pins against. Cannot import `crate::graph::dispatch::KNOWN_EDGE_TYPES` (that module lives
/// in the native `urban-integrity-workbench` crate, forbidden across this wasm-only crate's
/// boundary) — this is a deliberate, independently-maintained copy of the same eleven names.
const ALL_EDGE_TYPES: [&str; 11] = [
    "Directs",
    "AwardedTo",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
    "Represents",
    "IssuedBy",
    "LocatedAt",
];

/// The six algorithm identifiers (`AlgorithmRequest.algorithm`'s wire vocabulary, plan 44-01) and
/// their rail display labels, in the phase 44 UI spec's fixed rail order — the single source of truth
/// this module's `<select>`, [`algorithm_rail_label`] and every `"Running {label}…"`/failure-copy
/// site draw from.
const ALGORITHM_OPTIONS: [(&str, &str); 6] = [
    ("community", "Community detection"),
    ("k_core", "k-core"),
    ("betweenness", "Betweenness centrality"),
    ("all_simple_paths", "All simple paths"),
    ("shortest_paths", "Shortest / k-shortest paths"),
    ("ward_subgraph", "Ward-reachable subgraph"),
];

// ─── Ceiling help text (D-08, "never silently clamped without the ceiling being visible") ─────

const HOP_CAP_HELP: &str = "up to 8";
const RESULT_CAP_HELP: &str = "up to 1,000";
const BUDGET_SECS_HELP: &str = "up to 30s";
const K_CORE_K_HELP: &str = "up to 50";
const SHORTEST_K_HELP: &str = "up to 1,000";
const TOP_N_HELP: &str = "up to 500";
const MIN_COMMUNITY_SIZE_HELP: &str = "up to 500";
const WARD_HOPS_HELP: &str = "up to 8";
/// D-08: "seed… carries no ceiling (seeded is never a hidden constant)" — still shown, never
/// omitted, so the analyst never wonders whether a ceiling silently applies.
const SEED_HELP: &str = "No ceiling.";
/// D-08: "resolution carries no ceiling — a tuning parameter, not a safety cap."
const RESOLUTION_HELP: &str = "No ceiling — a tuning parameter, not a safety cap.";

/// The 8 edge types that start CHECKED (D-01) — `Represents`/`IssuedBy`/`LocatedAt` are the three
/// hub-forming types that start unchecked and are never included here.
pub fn default_edge_types_included() -> Vec<String> {
    [
        "Directs",
        "AwardedTo",
        "Controls",
        "TrusteeOf",
        "OwnsProperty",
        "OffshoreLink",
        "DonatesTo",
        "DisqualifiedFrom",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// D-01's forced-inclusion rule: ward-reachable subgraph always walks `Represents` regardless of
/// the checklist selection. The ONE place this rule is applied — both [`algorithm_defaults`]'s
/// pure default-state case and [`build_algorithm_request`]'s live-request case route through this
/// function, so the two can never drift apart. Never mutates the checklist's own persisted
/// selection (D-05) — the checkbox's rendered "checked" state is computed separately, at render
/// time, without ever writing `Represents` into the caller's real selection.
fn with_forced_edge_types(algorithm: &str, edge_types: &[String]) -> Vec<String> {
    if algorithm == "ward_subgraph" && !edge_types.iter().any(|t| t == "Represents") {
        let mut out = edge_types.to_vec();
        out.push("Represents".to_string());
        out
    } else {
        edge_types.to_vec()
    }
}

/// Builds the exact default `AlgorithmRequest` for `algorithm`, per the phase 44 UI spec's per-algorithm
/// parameter table — the transcription-accuracy proof this module's tests pin field-by-field.
/// `session_id` is always empty here (this function never sees the real per-tab session id,
/// minted by `graph_page.rs`); the algorithm-switch handler only reads this for the PARAMETER
/// fields, never `session_id`/`algorithm` itself, from its return value.
pub fn algorithm_defaults(algorithm: &str) -> AlgorithmRequest {
    let edge_types_included = with_forced_edge_types(algorithm, &default_edge_types_included());
    let mut request = AlgorithmRequest {
        algorithm: algorithm.to_string(),
        session_id: String::new(),
        edge_types_included,
        seed: caps::DEFAULT_SEED,
        from_id: None,
        to_id: None,
        ward_id: None,
        ids: vec![],
        // Every rail-built request scopes to the whole population — `finding_detail_page.rs`'s
        // scoped requests bypass this function and set `scope_ids` on the request it hands off to
        // `start_run` directly (see `ToolRailSignals::scope_ids`'s own doc comment).
        scope_ids: vec![],
        hop_cap: caps::DEFAULT_HOP_CAP,
        result_cap: caps::DEFAULT_RESULT_CAP,
        budget_secs: caps::DEFAULT_BUDGET_SECS,
        k: caps::DEFAULT_K_SHORTEST,
        top_n: caps::DEFAULT_TOP_N,
        min_community_size: caps::DEFAULT_MIN_COMMUNITY_SIZE,
        resolution: caps::DEFAULT_RESOLUTION,
    };
    match algorithm {
        "k_core" => request.k = caps::DEFAULT_K_CORE,
        "ward_subgraph" => request.hop_cap = caps::DEFAULT_WARD_SUBGRAPH_HOPS,
        _ => {}
    }
    request
}

/// The rail's display label for `algorithm` (used for the `"Running {label}…"` aria-live text and
/// the failure-copy line) — falls back to the raw identifier for a genuinely unrecognised value
/// rather than panicking (defensive; every real caller always passes one of the six known ids).
pub fn algorithm_rail_label(algorithm: &str) -> &str {
    ALGORITHM_OPTIONS
        .iter()
        .find(|(id, _)| *id == algorithm)
        .map(|(_, label)| *label)
        .unwrap_or(algorithm)
}

/// The Copywriting Contract's exact Run-failure format, extending the Analysis page's D-19
/// pattern to this page's own Run action: "The {algorithm} run failed: {error text from the
/// server}."
pub fn algorithm_run_failure_copy(algorithm: &str, server_error: &str) -> String {
    format!(
        "The {} run failed: {server_error}.",
        algorithm_rail_label(algorithm)
    )
}

/// Builds the `AlgorithmRequest` the Run button actually posts, from the rail's current signal
/// state (already read into plain values by the caller) — applies [`with_forced_edge_types`] so
/// the request's `edge_types_included` always reflects the D-01 forced-Represents rule for
/// ward-reachable subgraph, even though the checklist's own persisted selection (D-05) is never
/// mutated to represent it.
#[allow(clippy::too_many_arguments)]
pub fn build_algorithm_request(
    algorithm: &str,
    session_id: &str,
    edge_types_included: &[String],
    seed: u64,
    from_id: Option<String>,
    to_id: Option<String>,
    ward_id: Option<String>,
    hop_cap: u32,
    result_cap: u32,
    budget_secs: u32,
    k: u32,
    top_n: u32,
    min_community_size: u32,
    resolution: f64,
) -> AlgorithmRequest {
    AlgorithmRequest {
        algorithm: algorithm.to_string(),
        session_id: session_id.to_string(),
        edge_types_included: with_forced_edge_types(algorithm, edge_types_included),
        seed,
        from_id,
        to_id,
        ward_id,
        // `highlight_set` is deep-link only and never built here; every rail-built request
        // carries an empty set.
        ids: vec![],
        // Populated by the caller (`start_run`) from `ToolRailSignals::scope_ids` — this function
        // itself has no access to that signal, so it always starts whole-population here and
        // `start_run` overwrites it before posting (plan 50-11's scoped-run gap-closure fix).
        scope_ids: vec![],
        hop_cap,
        result_cap,
        budget_secs,
        k,
        top_n,
        min_community_size,
        resolution,
    }
}

/// Builds the `highlight_set` request a deep link produces (plan 44-21). Not reachable from the
/// rail — `ALGORITHM_OPTIONS` stays at six, because `highlight_set` answers no question the
/// analyst can pose from the rail; it exists so a finding elsewhere in the workbench can put its
/// own entities on the canvas and have the status line, cache and Reset behave exactly as they do
/// for a real algorithm run.
pub fn build_highlight_request(
    session_id: &str,
    ids: Vec<String>,
    edge_types_included: &[String],
) -> AlgorithmRequest {
    let mut request = algorithm_defaults("highlight_set");
    request.session_id = session_id.to_string();
    request.edge_types_included = edge_types_included.to_vec();
    request.ids = ids;
    request
}

/// The `AlgorithmRequest` [`start_run`] posts, read from `state`'s current signal values — factored
/// out as its own function (this crate's "plain-function-first" testing convention) so the 50-11
/// scoped-run copy (`request.scope_ids = state.scope_ids.get_untracked()`) is directly host-
/// testable without a DOM or a network round trip.
fn algorithm_request_for_run(state: ToolRailSignals) -> AlgorithmRequest {
    let algorithm = state.algorithm.get_untracked();
    let session_id = state.session_id.get_value();
    let edge_types = state.edge_types.get_untracked();
    let from_id = state.from_ref.get_untracked().map(|entity| entity.id);
    let to_id = state.to_ref.get_untracked().map(|entity| entity.id);
    let ward_id = state.ward_ref.get_untracked().map(|entity| entity.id);
    let mut request = build_algorithm_request(
        &algorithm,
        &session_id,
        &edge_types,
        state.seed.get_untracked(),
        from_id,
        to_id,
        ward_id,
        state.hop_cap.get_untracked(),
        state.result_cap.get_untracked(),
        state.budget_secs.get_untracked(),
        state.k.get_untracked(),
        state.top_n.get_untracked(),
        state.min_community_size.get_untracked(),
        state.resolution.get_untracked(),
    );
    // 50-11's scoped-run fix: a non-empty `scope_ids` (only ever set by the finding detail route)
    // narrows this run to that neighbourhood; the Graph page's default empty signal leaves every
    // Run exactly as it behaved before this plan.
    request.scope_ids = state.scope_ids.get_untracked();
    request
}

/// Start an algorithm run from the rail's current signals. Used by the Run button and by the
/// Graph page's deep-link application; a no-op while a run is in flight, so a deep link cannot
/// stack a second run on top of one the operator already started.
pub fn start_run(state: ToolRailSignals) {
    if state.algorithm_running.get_untracked() {
        return;
    }

    let request = algorithm_request_for_run(state);
    let algorithm = request.algorithm.clone();
    state.algorithm_running.set(true);
    let algorithm_for_error = algorithm.clone();
    let algorithm_result = state.algorithm_result;
    let algorithm_error = state.algorithm_error;
    let algorithm_running = state.algorithm_running;
    leptos::task::spawn_local(async move {
        match trigger_algorithm_run(&request).await {
            Ok(result) => {
                algorithm_result.set(Some(result));
                algorithm_error.set(None);
            }
            Err(msg) => {
                algorithm_error.set(Some(algorithm_run_failure_copy(&algorithm_for_error, &msg)));
            }
        }
        algorithm_running.set(false);
    });
}

/// Deep-link variant: run `highlight_set` over `ids` (spec §6.2, three or more refs). Separate
/// from [`start_run`] because the rail has no signals for a highlight set — the ids come from the
/// link, not from a picker field.
pub fn start_highlight(state: ToolRailSignals, ids: Vec<String>) {
    if state.algorithm_running.get_untracked() {
        return;
    }
    let request = build_highlight_request(
        &state.session_id.get_value(),
        ids,
        &state.edge_types.get_untracked(),
    );
    state.algorithm_running.set(true);
    let algorithm_result = state.algorithm_result;
    let algorithm_error = state.algorithm_error;
    let algorithm_running = state.algorithm_running;
    leptos::task::spawn_local(async move {
        match trigger_algorithm_run(&request).await {
            Ok(result) => {
                algorithm_result.set(Some(result));
                algorithm_error.set(None);
            }
            Err(msg) => {
                algorithm_error.set(Some(algorithm_run_failure_copy("highlight_set", &msg)));
            }
        }
        algorithm_running.set(false);
    });
}

/// `true` when Run should be enabled: `false` if `out_of_scope`; for the two paths algorithms,
/// `false` unless both `from_ref`/`to_ref` are resolved; for ward-reachable subgraph, `false`
/// unless `ward_ref` is resolved; otherwise `true` (community/k-core/betweenness need no picker
/// field, per the phase 44 UI spec's per-algorithm parameter table).
pub fn run_enabled(
    algorithm: &str,
    from_ref: &Option<EntityRef>,
    to_ref: &Option<EntityRef>,
    ward_ref: &Option<EntityRef>,
    out_of_scope: bool,
) -> bool {
    if out_of_scope {
        return false;
    }
    match algorithm {
        "all_simple_paths" | "shortest_paths" => from_ref.is_some() && to_ref.is_some(),
        "ward_subgraph" => ward_ref.is_some(),
        _ => true,
    }
}

/// `true` when any picker field REQUIRED by `algorithm` holds a resolved entity whose id is not
/// present in `population_node_ids` — the T-44-11-02 out-of-scope check this rail owns (per
/// `EntityPicker`'s own doc comment: "the caller performs out-of-scope check"). A field the
/// current algorithm does not require is never checked, even if it happens to hold a stale
/// resolved value.
pub fn any_required_ref_out_of_scope(
    algorithm: &str,
    from_ref: &Option<EntityRef>,
    to_ref: &Option<EntityRef>,
    ward_ref: &Option<EntityRef>,
    population_node_ids: &[String],
) -> bool {
    let out_of_scope = |entity: &Option<EntityRef>| {
        entity
            .as_ref()
            .is_some_and(|e| !population_node_ids.iter().any(|id| id == &e.id))
    };
    match algorithm {
        "all_simple_paths" | "shortest_paths" => out_of_scope(from_ref) || out_of_scope(to_ref),
        "ward_subgraph" => out_of_scope(ward_ref),
        _ => false,
    }
}

/// Posts `request` to [`ALGORITHM_URL`]. `200` -> the new `AlgorithmResult`; any other status
/// reads the response's `error` field where present, mirroring `analysis_page.rs::trigger_run`'s
/// exact transport shape.
async fn trigger_algorithm_run(request: &AlgorithmRequest) -> Result<AlgorithmResult, String> {
    let response = gloo_net::http::Request::post(ALGORITHM_URL)
        .json(request)
        .map_err(|err| err.to_string())?
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status() == 200 {
        return response
            .json::<AlgorithmResult>()
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
    Err(body_error.unwrap_or_else(|| {
        format!(
            "the algorithm run request returned an unexpected response (status {})",
            response.status()
        )
    }))
}

/// Posts `{"session_id": session_id}` to [`ALGORITHM_CANCEL_URL`] — always 200, idempotent (plan
/// 44-09's own contract). `graph_page.rs`'s page-leave cancellation fires this fire-and-forget
/// (never awaited by its caller); Cancel run's own click handler awaits it only so the button's
/// own request has actually left the browser before the outstanding Run's response settles
/// `algorithm_running` back to `false`.
pub(crate) async fn trigger_algorithm_cancel(session_id: String) {
    let body = serde_json::json!({ "session_id": session_id });
    if let Ok(request) = gloo_net::http::Request::post(ALGORITHM_CANCEL_URL).json(&body) {
        let _ = request.send().await;
    }
}

/// Every reactive signal the rail reads/writes, grouped into one `Copy` struct per this plan's
/// own stated discretion ("group into a struct if 13 individual props feels unwieldy") —
/// `graph_page.rs` owns creation of every field; each is independently settable since the
/// per-algorithm parameter forms edit them one at a time.
#[derive(Clone, Copy)]
pub struct ToolRailSignals {
    pub algorithm: RwSignal<String>,
    pub edge_types: RwSignal<Vec<String>>,
    pub from_ref: RwSignal<Option<EntityRef>>,
    pub to_ref: RwSignal<Option<EntityRef>>,
    pub ward_ref: RwSignal<Option<EntityRef>>,
    pub seed: RwSignal<u64>,
    pub hop_cap: RwSignal<u32>,
    pub result_cap: RwSignal<u32>,
    pub budget_secs: RwSignal<u32>,
    pub k: RwSignal<u32>,
    pub top_n: RwSignal<u32>,
    pub min_community_size: RwSignal<u32>,
    pub resolution: RwSignal<f64>,
    pub session_id: StoredValue<String, LocalStorage>,
    pub algorithm_result: RwSignal<Option<AlgorithmResult>>,
    pub algorithm_running: RwSignal<bool>,
    pub algorithm_error: RwSignal<Option<String>>,
    /// The full population's node ids (`graph_page.rs`'s already-parsed `node_ids`, reused here
    /// per the phase 44 UI spec's "Out-of-scope handling": "a small client-side set the
    /// population-metadata fetch already has") — the out-of-scope check's own scope set.
    pub population_node_ids: StoredValue<Vec<String>, LocalStorage>,
    /// Restricts every rail-built run to this node-id subset, copied into each posted
    /// `AlgorithmRequest.scope_ids` by [`start_run`]. Empty (`graph_page.rs`'s default) means "run
    /// over the whole population" — unchanged Graph-page behaviour. The finding detail route
    /// (`finding_detail_page.rs`) sets this to the finding's own resolved neighbourhood ids once
    /// that fetch resolves, so a Run posted from `/finding/:probe` is scoped to exactly the
    /// neighbourhood the canvas draws, and the status line's counts describe that same set (plan
    /// 50-11's gap-closure fix for the operator's "3 communities but I can only see ~9 nodes"
    /// report).
    pub scope_ids: RwSignal<Vec<String>>,
}

fn number_field_u32(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    value: RwSignal<u32>,
) -> impl IntoView {
    let describedby = format!("{id}-help");
    let describedby_for_span = describedby.clone();
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=id>{label}</label>
            <input
                id=id
                type="number"
                aria-describedby=describedby
                prop:value=move || value.get().to_string()
                on:input=move |ev| {
                    if let Ok(parsed) = event_target_value(&ev).parse::<u32>() {
                        value.set(parsed);
                    }
                }
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            <span id=describedby_for_span class="text-label" style="color: var(--muted-foreground);">{help}</span>
        </div>
    }
}

fn number_field_u64(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    value: RwSignal<u64>,
) -> impl IntoView {
    let describedby = format!("{id}-help");
    let describedby_for_span = describedby.clone();
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=id>{label}</label>
            <input
                id=id
                type="number"
                aria-describedby=describedby
                prop:value=move || value.get().to_string()
                on:input=move |ev| {
                    if let Ok(parsed) = event_target_value(&ev).parse::<u64>() {
                        value.set(parsed);
                    }
                }
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            <span id=describedby_for_span class="text-label" style="color: var(--muted-foreground);">{help}</span>
        </div>
    }
}

/// A typed `f64` parameter field with its ceiling (or its absence) stated beneath it.
///
/// `pub` since plan 45-14: the canvas filters' `P_c` threshold is the same control with a
/// different bound, and a second copy of it in `canvas_filters.rs` is how one screen comes to have
/// two number fields that behave differently.
pub fn number_field_f64(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    value: RwSignal<f64>,
) -> impl IntoView {
    let describedby = format!("{id}-help");
    let describedby_for_span = describedby.clone();
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=id>{label}</label>
            <input
                id=id
                type="number"
                step="any"
                aria-describedby=describedby
                prop:value=move || value.get().to_string()
                on:input=move |ev| {
                    if let Ok(parsed) = event_target_value(&ev).parse::<f64>() {
                        value.set(parsed);
                    }
                }
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            <span id=describedby_for_span class="text-label" style="color: var(--muted-foreground);">{help}</span>
        </div>
    }
}

/// A From/To/Ward `EntityPicker` field: the label, the picker itself, and — once a resolved chip
/// is out of the current graph population — the [`out_of_scope_copy`] error line beneath it
/// (T-44-11-02, the phase 44 UI spec "Rail — out-of-scope pick").
fn picker_field(
    label: &'static str,
    value: RwSignal<Option<EntityRef>>,
    population_node_ids: StoredValue<Vec<String>, LocalStorage>,
) -> impl IntoView {
    let on_select = move |entity: EntityRef| value.set(Some(entity));
    // Clearing the chip must clear the BOUND ref too, not just the picker's own display — see
    // `EntityPicker`'s `on_clear` doc comment.
    let on_clear = move || value.set(None);
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <span class="text-label">{label}</span>
            <EntityPicker
                placeholder=PICKER_PLACEHOLDER.to_string()
                on_select=on_select
                on_clear=on_clear
            />
            {move || {
                value.get().and_then(|entity| {
                    let in_scope = population_node_ids
                        .with_value(|ids| ids.iter().any(|id| id == &entity.id));
                    (!in_scope).then(|| view! {
                        <span class="text-body" style="color: var(--destructive);">
                            {out_of_scope_copy(&entity.name)}
                        </span>
                    })
                })
            }}
        </div>
    }
}

/// The per-algorithm parameter form (the phase 44 UI spec's "Per-algorithm parameter table"), reactive
/// on `state.algorithm` — renders only the fields that algorithm's row lists, each with its
/// ceiling stated as visible help text, never silently clamped without the ceiling being shown.
fn parameter_form(state: ToolRailSignals) -> impl IntoView {
    move || {
        let algorithm = state.algorithm.get();
        match algorithm.as_str() {
            "community" => view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                    {number_field_u64("community-seed", "Seed", SEED_HELP, state.seed)}
                    {number_field_u32("community-min-size", "Minimum community size", MIN_COMMUNITY_SIZE_HELP, state.min_community_size)}
                    {number_field_f64("community-resolution", "Resolution", RESOLUTION_HELP, state.resolution)}
                </div>
            }.into_any(),
            "k_core" => view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                    {number_field_u32("k-core-k", "k", K_CORE_K_HELP, state.k)}
                </div>
            }.into_any(),
            "betweenness" => view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                    <span class="text-label" style="color: var(--muted-foreground);">{BETWEENNESS_PIVOT_HELP}</span>
                    {number_field_u32("betweenness-top-n", "Top N", TOP_N_HELP, state.top_n)}
                    {number_field_u64("betweenness-seed", "Seed", SEED_HELP, state.seed)}
                </div>
            }.into_any(),
            "all_simple_paths" => view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                    {picker_field("From", state.from_ref, state.population_node_ids)}
                    {picker_field("To", state.to_ref, state.population_node_ids)}
                    {number_field_u32("paths-hop-cap", "Hop cap", HOP_CAP_HELP, state.hop_cap)}
                    {number_field_u32("paths-result-cap", "Result cap", RESULT_CAP_HELP, state.result_cap)}
                    {number_field_u32("paths-budget-secs", "Wall-clock budget (seconds)", BUDGET_SECS_HELP, state.budget_secs)}
                </div>
            }.into_any(),
            "shortest_paths" => view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                    {picker_field("From", state.from_ref, state.population_node_ids)}
                    {picker_field("To", state.to_ref, state.population_node_ids)}
                    {number_field_u32("shortest-k", "k", SHORTEST_K_HELP, state.k)}
                </div>
            }.into_any(),
            "ward_subgraph" => view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                    {picker_field("Ward", state.ward_ref, state.population_node_ids)}
                    {number_field_u32("ward-hops", "Hops", WARD_HOPS_HELP, state.hop_cap)}
                </div>
            }.into_any(),
            _ => ().into_any(),
        }
    }
}

/// The 11-row edge-type checklist (D-01, D-02, D-04, D-05): initial state from
/// `state.edge_types.get_untracked()` (already seeded to [`default_edge_types_included`] by
/// `graph_page.rs`), persisted across algorithm switches. When `ward_subgraph` is selected, the
/// `Represents` row renders checked and disabled with [`WARD_REPRESENTS_FORCED_TEXT`] beneath it
/// — the checklist's own underlying `state.edge_types` value is never mutated to reflect this;
/// [`with_forced_edge_types`] applies the forced inclusion only when a request is actually built.
fn edge_type_checklist(state: ToolRailSignals) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: 2px;">
            {ALL_EDGE_TYPES.iter().map(|&edge_type| {
                let edge_type_owned = edge_type.to_string();
                let edge_type_for_checked = edge_type_owned.clone();
                let edge_type_for_click = edge_type_owned.clone();
                let checked = Signal::derive(move || {
                    let forced = state.algorithm.get() == "ward_subgraph" && edge_type == "Represents";
                    forced || state.edge_types.get().iter().any(|t| t == &edge_type_for_checked)
                });
                let disabled = Signal::derive(move || {
                    state.algorithm.get() == "ward_subgraph" && edge_type == "Represents"
                });
                let help = Signal::derive(move || {
                    (state.algorithm.get() == "ward_subgraph" && edge_type == "Represents")
                        .then(|| WARD_REPRESENTS_FORCED_TEXT.to_string())
                });
                checklist_row(edge_type.to_string(), checked, disabled, help, move |checked| {
                    state.edge_types.update(|types| {
                        if checked {
                            if !types.iter().any(|t| t == &edge_type_for_click) {
                                types.push(edge_type_for_click.clone());
                            }
                        } else {
                            types.retain(|t| t != &edge_type_for_click);
                        }
                    });
                })
            }).collect_view()}
        </div>
    }
}

/// ONE checklist row: a 44px-tall label-wrapped checkbox with optional inline help beneath it.
///
/// Extracted from [`edge_type_checklist`] by plan 45-14 so the canvas filters' node-type and
/// edge-type checklists **extend this idiom** rather than growing a second one beside it on the
/// same page. Both callers render through this function, so the two checklists on the Graph page
/// cannot come to look or behave differently.
pub fn checklist_row(
    label: String,
    checked: Signal<bool>,
    disabled: Signal<bool>,
    help: Signal<Option<String>>,
    on_toggle: impl Fn(bool) + 'static,
) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-height: 44px; padding: var(--space-xs) 0;">
            <label style="display: flex; align-items: center; gap: var(--space-xs); min-height: 44px; cursor: pointer;">
                <input
                    type="checkbox"
                    checked=move || checked.get()
                    disabled=move || disabled.get()
                    on:change=move |ev| on_toggle(event_target_checked(&ev))
                />
                <span class="text-label">{label}</span>
            </label>
            {move || help.get().map(|text| view! {
                <span class="text-label" style="color: var(--muted-foreground); padding-left: calc(16px + var(--space-xs));">
                    {text}
                </span>
            })}
        </div>
    }
}

/// A `YYYY-MM-DD` date field, empty for "no bound".
///
/// The fourth member of this module's typed-field family ([`number_field_u32`],
/// [`number_field_u64`], [`number_field_f64`]), added by plan 45-14 for CNV-03's date range. A
/// native `<input type="date">` rather than a parsed text box **is** the no-DSL requirement at the
/// level of one control: there is no string here for anything to interpret.
pub fn date_field(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    value: RwSignal<Option<String>>,
) -> impl IntoView {
    let describedby = format!("{id}-help");
    let describedby_for_span = describedby.clone();
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=id>{label}</label>
            <input
                id=id
                type="date"
                aria-describedby=describedby
                prop:value=move || value.get().unwrap_or_default()
                on:input=move |ev| {
                    let raw = event_target_value(&ev);
                    // An emptied field is "no bound", never an empty string the server would have
                    // to decide the meaning of.
                    value.set((!raw.trim().is_empty()).then_some(raw));
                }
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            <span id=describedby_for_span class="text-label" style="color: var(--muted-foreground);">{help}</span>
        </div>
    }
}

/// The phase 44 UI spec "Canvas legend": "When a community result is active, this block is replaced by
/// the generated community-colour list" (each results-list community row already carries its own
/// swatch, plan 44-13's `graph_results_list.rs` — the legend and the results list would otherwise
/// show the same colour information twice). `true` exactly when `result` holds a `Community`
/// outcome — the ONE gap the plan 44-12 summary's Known Limitations explicitly deferred to this plan
/// ("Canvas legend does not yet swap to the community-colour list... the swap depends on the
/// results list, which is plan 44-13's job").
fn is_community_result_active(result: &Option<AlgorithmResult>) -> bool {
    result
        .as_ref()
        .is_some_and(|r| matches!(r.outcome, AlgorithmOutcome::Community { .. }))
}

/// The static canvas legend (D-15): the six core node-kind swatches from
/// [`node_palette::NODE_KIND_PALETTE`](crate::node_palette::NODE_KIND_PALETTE) plus the shared
/// OSINT swatch's label rendered exactly per the phase 44 UI spec's "Canvas legend" section — both
/// `OSINT` and `PSC` through `<Abbr>` since both are glossary tokens (D-03/NAV-04). Rendered only
/// when [`is_community_result_active`] is `false` — see that function's doc comment.
fn canvas_legend() -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            {NODE_KIND_PALETTE.iter()
                .filter(|(label, _)| *label != OSINT_ABBR)
                .map(|(label, token)| view! {
                    <div style="display: flex; align-items: center; gap: var(--space-xs);">
                        <span aria-hidden="true" style=format!("width: 12px; height: 12px; border-radius: 9999px; flex-shrink: 0; background: var({token});")></span>
                        <span class="text-label">{*label}</span>
                    </div>
                }).collect_view()}
            <div style="display: flex; align-items: center; gap: var(--space-xs);">
                <span aria-hidden="true" style="width: 12px; height: 12px; border-radius: 9999px; flex-shrink: 0; background: var(--node-osint);"></span>
                <span class="text-label">
                    <Abbr token=OSINT_ABBR.to_string() />
                    " — Political actor, "
                    <Abbr token=PSC_ABBR.to_string() />
                    ", Charity, Offshore entity, Disqualification"
                </span>
            </div>
            <div class="text-label" style="margin-top: var(--space-xs); color: var(--muted-foreground);">
                "Link colour — edge type"
            </div>
            {EDGE_TYPE_PALETTE.iter()
                .map(|(label, token, members)| view! {
                    <div style="display: flex; align-items: center; gap: var(--space-xs);">
                        <span aria-hidden="true" style=format!("width: 12px; height: 3px; border-radius: 2px; flex-shrink: 0; background: var({token});")></span>
                        <span class="text-label">
                            {*label}
                            " — "
                            {members.join(", ")}
                        </span>
                    </div>
                }).collect_view()}
        </div>
    }
}

/// The Run/Cancel run button (44×44px min-height, full rail width) plus its `aria-live="polite"`
/// status region — Copywriting Contract: idle "Run" (disabled per [`run_enabled`]/
/// [`any_required_ref_out_of_scope`]), running "Cancel run" in the SAME slot, "Running
/// {algorithm}…" while running, or the [`algorithm_run_failure_copy`] line on failure (never
/// clearing the previous result — `state.algorithm_result` is untouched on `Err`).
fn run_control(state: ToolRailSignals) -> impl IntoView {
    let run_onclick = move |_: web_sys::MouseEvent| start_run(state);

    let cancel_onclick = move |_: web_sys::MouseEvent| {
        let session_id = state.session_id.get_value();
        leptos::task::spawn_local(async move {
            trigger_algorithm_cancel(session_id).await;
        });
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <button
                class="text-body"
                style="background: var(--primary); color: var(--primary-foreground); border: none; border-radius: 6px; padding: var(--space-xs) var(--space-md); cursor: pointer; min-height: 44px; width: 100%;"
                disabled=move || {
                    if state.algorithm_running.get() {
                        return false;
                    }
                    let algorithm = state.algorithm.get();
                    let from_ref = state.from_ref.get();
                    let to_ref = state.to_ref.get();
                    let ward_ref = state.ward_ref.get();
                    let out_of_scope = state.population_node_ids.with_value(|ids| {
                        any_required_ref_out_of_scope(&algorithm, &from_ref, &to_ref, &ward_ref, ids)
                    });
                    !run_enabled(&algorithm, &from_ref, &to_ref, &ward_ref, out_of_scope)
                }
                on:click=move |ev| {
                    if state.algorithm_running.get_untracked() {
                        cancel_onclick(ev);
                    } else {
                        run_onclick(ev);
                    }
                }
            >
                {move || if state.algorithm_running.get() { "Cancel run" } else { "Run" }}
            </button>
            <div aria-live="polite" style="display: flex; flex-direction: column; gap: var(--space-xs);">
                {move || {
                    if state.algorithm_running.get() {
                        let algorithm = state.algorithm.get();
                        Some(view! {
                            <span class="text-label" style="color: var(--muted-foreground);">
                                {format!("Running {}\u{2026}", algorithm_rail_label(&algorithm))}
                            </span>
                        }.into_any())
                    } else {
                        state.algorithm_error.get().map(|msg| view! {
                            <span class="text-body" style="color: var(--destructive);">{msg}</span>
                        }.into_any())
                    }
                }}
            </div>
        </div>
    }
}

/// The tool rail (GRA-01–GRA-04, GRA-06): algorithm picker, per-algorithm parameter form,
/// edge-type checklist, canvas legend, Run/Cancel run control. Switching the `<select>` resets
/// every parameter signal to [`algorithm_defaults`]'s corresponding field (never carrying a stale
/// value from a different algorithm's form into the new one), clears any active
/// `algorithm_result`/`algorithm_error` in the SAME handler (the phase 44 UI spec: "switching the
/// algorithm selector immediately clears any active result"), but LEAVES `edge_types` untouched
/// (D-05: persists across the switch).
#[component]
#[allow(non_snake_case)]
pub fn ToolRail(state: ToolRailSignals) -> impl IntoView {
    let on_algorithm_change = move |ev: web_sys::Event| {
        let new_algorithm = event_target_value(&ev);
        let defaults = algorithm_defaults(&new_algorithm);
        state.seed.set(defaults.seed);
        state.hop_cap.set(defaults.hop_cap);
        state.result_cap.set(defaults.result_cap);
        state.budget_secs.set(defaults.budget_secs);
        state.k.set(defaults.k);
        state.top_n.set(defaults.top_n);
        state.min_community_size.set(defaults.min_community_size);
        state.resolution.set(defaults.resolution);
        state.from_ref.set(None);
        state.to_ref.set(None);
        state.ward_ref.set(None);
        // `state.edge_types` is intentionally left untouched here — D-05's persisted selection
        // survives an algorithm switch.
        state.algorithm_result.set(None);
        state.algorithm_error.set(None);
        state.algorithm.set(new_algorithm);
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-md); padding: var(--space-lg); background: var(--card); border: 1px solid var(--border); border-radius: 8px;">
            <select
                class="text-body"
                aria-label="Algorithm"
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
                prop:value=move || state.algorithm.get()
                on:change=on_algorithm_change
            >
                {ALGORITHM_OPTIONS.iter().map(|(value, label)| view! {
                    <option value=*value>{*label}</option>
                }).collect_view()}
            </select>

            <a
                class="text-label"
                href=move || format!("/help/algorithms/{}", state.algorithm.get())
            >
                "Help for this algorithm"
            </a>

            {parameter_form(state)}

            {edge_type_checklist(state)}

            {move || (!is_community_result_active(&state.algorithm_result.get())).then(canvas_legend)}

            {run_control(state)}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{AlgorithmCaps, Truncation};

    // ─── is_community_result_active (plan 44-13's deferred legend swap) ────────────────────

    fn sample_result(outcome: AlgorithmOutcome) -> AlgorithmResult {
        AlgorithmResult {
            outcome,
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: None,
                result_cap: None,
                budget_secs: None,
                k: None,
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            wall_clock_ms: 0,
        }
    }

    #[test]
    fn is_community_result_active_true_only_for_a_community_outcome() {
        assert!(!is_community_result_active(&None));
        assert!(is_community_result_active(&Some(sample_result(
            AlgorithmOutcome::Community {
                communities: vec![],
                hidden_node_ids: vec![],
            }
        ))));
        assert!(!is_community_result_active(&Some(sample_result(
            AlgorithmOutcome::KCore {
                k: 3,
                node_count: 0,
                edge_count: 0,
                top_nodes: vec![],
                hidden_node_ids: vec![],
            }
        ))));
    }

    // ─── default_edge_types_included ───────────────────────────────────────────────────────

    #[test]
    fn default_edge_types_included_is_exactly_the_eight_default_checked_types() {
        let defaults = default_edge_types_included();
        assert_eq!(defaults.len(), 8);
        assert_eq!(
            defaults,
            vec![
                "Directs",
                "AwardedTo",
                "Controls",
                "TrusteeOf",
                "OwnsProperty",
                "OffshoreLink",
                "DonatesTo",
                "DisqualifiedFrom",
            ]
        );
    }

    /// PROVE THIS CAN FAIL: a version of `default_edge_types_included` that accidentally included
    /// one of the three hub-forming types would still pass a length-8 check if it dropped a
    /// different entry — this asserts the three types are genuinely ABSENT, not merely that the
    /// count happens to be 8.
    #[test]
    fn default_edge_types_included_never_includes_the_three_hub_forming_types() {
        let defaults = default_edge_types_included();
        for hub_forming in ["Represents", "IssuedBy", "LocatedAt"] {
            assert!(
                !defaults.iter().any(|t| t == hub_forming),
                "{hub_forming} must start unchecked (D-01)"
            );
        }
    }

    // ─── algorithm_defaults — one assertion per algorithm per the parameter table ──────────

    /// The rail's ceiling copy and the server's clamp must state the same number. These strings
    /// are hand-written prose ("up to 1,000"), so nothing but this test stops one from drifting
    /// away from the clamp in `dispatch::clamp_request` — help that promises a ceiling the server
    /// does not enforce (or hides one it does) is worse than no help.
    #[test]
    fn ceiling_help_strings_match_caps() {
        use urban_integrity_workbench_dto::caps::*;
        assert!(HOP_CAP_HELP.contains(&CEILING_HOP_CAP.to_string()));
        assert!(WARD_HOPS_HELP.contains(&CEILING_HOP_CAP.to_string()));
        assert!(RESULT_CAP_HELP
            .replace(',', "")
            .contains(&CEILING_RESULT_CAP.to_string()));
        assert!(BUDGET_SECS_HELP.contains(&CEILING_BUDGET_SECS.to_string()));
        assert!(K_CORE_K_HELP.contains(&CEILING_K_CORE.to_string()));
        assert!(SHORTEST_K_HELP
            .replace(',', "")
            .contains(&CEILING_K_SHORTEST.to_string()));
        assert!(TOP_N_HELP.contains(&CEILING_TOP_N.to_string()));
        assert!(MIN_COMMUNITY_SIZE_HELP.contains(&CEILING_MIN_COMMUNITY_SIZE.to_string()));
        assert!(BETWEENNESS_PIVOT_HELP.contains(&BETWEENNESS_PIVOTS.to_string()));
    }

    /// PROVE THIS CAN FAIL: the assertions above are substring checks, so a help string of
    /// "up to 8000" would satisfy the hop-cap one. This pins the exact copy for the two fields
    /// whose ceilings share digits with a plausible wrong number.
    #[test]
    fn ceiling_help_strings_are_exact_not_merely_containing() {
        assert_eq!(HOP_CAP_HELP, "up to 8");
        assert_eq!(RESULT_CAP_HELP, "up to 1,000");
    }

    /// The rail's defaults must be the shared constants, not re-typed numbers — including the
    /// two per-algorithm overrides, which are the easiest to get wrong because they share a
    /// request field with a different algorithm's default.
    #[test]
    fn algorithm_defaults_come_from_caps() {
        use urban_integrity_workbench_dto::caps::*;
        let community = algorithm_defaults("community");
        assert_eq!(community.seed, DEFAULT_SEED);
        assert_eq!(community.hop_cap, DEFAULT_HOP_CAP);
        assert_eq!(community.result_cap, DEFAULT_RESULT_CAP);
        assert_eq!(community.budget_secs, DEFAULT_BUDGET_SECS);
        assert_eq!(community.top_n, DEFAULT_TOP_N);
        assert_eq!(community.min_community_size, DEFAULT_MIN_COMMUNITY_SIZE);
        assert_eq!(community.resolution, DEFAULT_RESOLUTION);
        assert_eq!(community.k, DEFAULT_K_SHORTEST);

        assert_eq!(algorithm_defaults("k_core").k, DEFAULT_K_CORE);
        assert_eq!(
            algorithm_defaults("ward_subgraph").hop_cap,
            DEFAULT_WARD_SUBGRAPH_HOPS
        );
        assert_ne!(
            DEFAULT_K_CORE, DEFAULT_K_SHORTEST,
            "the two k defaults differ; one constant for both would silently change k-core"
        );
    }

    #[test]
    fn algorithm_defaults_for_community_matches_the_parameter_table() {
        let request = algorithm_defaults("community");
        assert_eq!(request.seed, 42);
        assert_eq!(request.min_community_size, 5);
        assert_eq!(request.resolution, 1.0);
        assert_eq!(request.from_id, None);
        assert_eq!(request.to_id, None);
        assert_eq!(request.ward_id, None);
        assert_eq!(request.edge_types_included, default_edge_types_included());
    }

    #[test]
    fn algorithm_defaults_for_k_core_matches_the_parameter_table() {
        let request = algorithm_defaults("k_core");
        assert_eq!(request.k, 2);
    }

    #[test]
    fn algorithm_defaults_for_betweenness_matches_the_parameter_table() {
        let request = algorithm_defaults("betweenness");
        assert_eq!(request.top_n, 50);
        assert_eq!(request.seed, 42);
    }

    #[test]
    fn algorithm_defaults_for_all_simple_paths_matches_the_parameter_table() {
        let request = algorithm_defaults("all_simple_paths");
        assert_eq!(request.hop_cap, 4);
        assert_eq!(request.result_cap, 100);
        assert_eq!(request.budget_secs, 5);
        assert_eq!(request.from_id, None);
        assert_eq!(request.to_id, None);
    }

    #[test]
    fn algorithm_defaults_for_shortest_paths_matches_the_parameter_table() {
        let request = algorithm_defaults("shortest_paths");
        assert_eq!(request.k, 3);
        // "internally reuses All-simple-paths' hop cap 4 (ceiling 8) and budget 5s".
        assert_eq!(request.hop_cap, 4);
        assert_eq!(request.result_cap, 100);
        assert_eq!(request.budget_secs, 5);
        assert_eq!(request.from_id, None);
        assert_eq!(request.to_id, None);
    }

    #[test]
    fn algorithm_defaults_for_ward_subgraph_matches_the_parameter_table() {
        let request = algorithm_defaults("ward_subgraph");
        assert_eq!(request.hop_cap, 2, "Hops defaults to 2, not the shared 4");
        assert_eq!(request.ward_id, None);
    }

    /// D-01 forced-inclusion: `algorithm_defaults("ward_subgraph")`'s edge-type field always
    /// includes `Represents`, even though [`default_edge_types_included`] itself never does.
    #[test]
    fn algorithm_defaults_for_ward_subgraph_forces_represents_included() {
        let request = algorithm_defaults("ward_subgraph");
        assert!(request
            .edge_types_included
            .iter()
            .any(|t| t == "Represents"));
        assert!(
            !default_edge_types_included()
                .iter()
                .any(|t| t == "Represents"),
            "sanity: the default checklist itself never includes Represents"
        );
    }

    /// Non-ward algorithms never force `Represents` in.
    #[test]
    fn algorithm_defaults_for_non_ward_algorithms_never_forces_represents() {
        for algorithm in [
            "community",
            "k_core",
            "betweenness",
            "all_simple_paths",
            "shortest_paths",
        ] {
            let request = algorithm_defaults(algorithm);
            assert!(
                !request
                    .edge_types_included
                    .iter()
                    .any(|t| t == "Represents"),
                "{algorithm} must not force Represents"
            );
        }
    }

    // ─── BETWEENNESS_PIVOT_HELP ─────────────────────────────────────────────────────────────

    #[test]
    fn betweenness_pivot_help_matches_the_ui_spec_verbatim() {
        assert_eq!(
            BETWEENNESS_PIVOT_HELP,
            "Approximate: samples 500 source nodes, seeded."
        );
    }

    // ─── with_forced_edge_types / build_algorithm_request ──────────────────────────────────

    #[test]
    fn build_algorithm_request_matches_the_checklist_signal_exactly_for_non_ward_algorithms() {
        let checklist = default_edge_types_included();
        let request = build_algorithm_request(
            "community",
            "session-abc",
            &checklist,
            42,
            None,
            None,
            None,
            4,
            100,
            5,
            3,
            50,
            5,
            1.0,
        );
        assert_eq!(request.edge_types_included, checklist);
    }

    /// PROVE THIS CAN FAIL: build the SAME checklist (no `Represents`) for `ward_subgraph` and
    /// confirm the forced inclusion is applied at request-build time, not just in
    /// `algorithm_defaults`'s own pure default state.
    #[test]
    fn build_algorithm_request_forces_represents_for_ward_subgraph_even_when_checklist_lacks_it() {
        let checklist = default_edge_types_included();
        assert!(!checklist.iter().any(|t| t == "Represents"));
        let request = build_algorithm_request(
            "ward_subgraph",
            "session-abc",
            &checklist,
            42,
            None,
            None,
            Some("ward:X05001499".to_string()),
            2,
            100,
            5,
            3,
            50,
            5,
            1.0,
        );
        assert!(request
            .edge_types_included
            .iter()
            .any(|t| t == "Represents"));
    }

    #[test]
    fn build_algorithm_request_does_not_duplicate_represents_if_already_checked() {
        let mut checklist = default_edge_types_included();
        checklist.push("Represents".to_string());
        let request = build_algorithm_request(
            "ward_subgraph",
            "session-abc",
            &checklist,
            42,
            None,
            None,
            Some("ward:X05001499".to_string()),
            2,
            100,
            5,
            3,
            50,
            5,
            1.0,
        );
        let represents_count = request
            .edge_types_included
            .iter()
            .filter(|t| *t == "Represents")
            .count();
        assert_eq!(represents_count, 1);
    }

    #[test]
    fn build_algorithm_request_carries_session_id_and_algorithm_through() {
        let request = build_algorithm_request(
            "k_core",
            "session-xyz",
            &default_edge_types_included(),
            42,
            None,
            None,
            None,
            4,
            100,
            5,
            2,
            50,
            5,
            1.0,
        );
        assert_eq!(request.algorithm, "k_core");
        assert_eq!(request.session_id, "session-xyz");
    }

    // ─── run_enabled — every algorithm's required-field combination ────────────────────────

    fn entity(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: "Some Entity".to_string(),
            kind: "Entity".to_string(),
        }
    }

    #[test]
    fn run_enabled_is_true_for_community_k_core_betweenness_with_no_picker_field() {
        for algorithm in ["community", "k_core", "betweenness"] {
            assert!(run_enabled(algorithm, &None, &None, &None, false));
        }
    }

    #[test]
    fn run_enabled_requires_both_from_and_to_for_paths_algorithms() {
        for algorithm in ["all_simple_paths", "shortest_paths"] {
            assert!(!run_enabled(algorithm, &None, &None, &None, false));
            assert!(!run_enabled(
                algorithm,
                &Some(entity("director:jane-smith")),
                &None,
                &None,
                false
            ));
            assert!(!run_enabled(
                algorithm,
                &None,
                &Some(entity("GB-COH:99000001")),
                &None,
                false
            ));
            assert!(run_enabled(
                algorithm,
                &Some(entity("director:jane-smith")),
                &Some(entity("GB-COH:99000001")),
                &None,
                false
            ));
        }
    }

    #[test]
    fn run_enabled_requires_ward_for_ward_subgraph() {
        assert!(!run_enabled("ward_subgraph", &None, &None, &None, false));
        assert!(run_enabled(
            "ward_subgraph",
            &None,
            &None,
            &Some(entity("ward:X05001499")),
            false
        ));
    }

    #[test]
    fn run_enabled_is_false_whenever_out_of_scope_regardless_of_algorithm() {
        assert!(!run_enabled("community", &None, &None, &None, true));
        assert!(!run_enabled(
            "all_simple_paths",
            &Some(entity("director:jane-smith")),
            &Some(entity("GB-COH:99000001")),
            &None,
            true
        ));
        assert!(!run_enabled(
            "ward_subgraph",
            &None,
            &None,
            &Some(entity("ward:X05001499")),
            true
        ));
    }

    // ─── any_required_ref_out_of_scope ──────────────────────────────────────────────────────

    #[test]
    fn out_of_scope_check_ignores_fields_the_current_algorithm_does_not_require() {
        let population = vec!["director:jane-smith".to_string()];
        // `from_ref` holds an out-of-scope id, but `community` never requires a picker field.
        assert!(!any_required_ref_out_of_scope(
            "community",
            &Some(entity("GB-COH:not-in-population")),
            &None,
            &None,
            &population,
        ));
    }

    #[test]
    fn out_of_scope_check_flags_a_from_or_to_id_missing_from_the_population() {
        let population = vec!["director:jane-smith".to_string()];
        assert!(any_required_ref_out_of_scope(
            "all_simple_paths",
            &Some(entity("GB-COH:not-in-population")),
            &Some(entity("director:jane-smith")),
            &None,
            &population,
        ));
        assert!(!any_required_ref_out_of_scope(
            "all_simple_paths",
            &Some(entity("director:jane-smith")),
            &Some(entity("director:jane-smith")),
            &None,
            &population,
        ));
    }

    #[test]
    fn out_of_scope_check_flags_a_ward_id_missing_from_the_population() {
        let population = vec!["ward:X05001499".to_string()];
        assert!(any_required_ref_out_of_scope(
            "ward_subgraph",
            &None,
            &None,
            &Some(entity("ward:not-in-population")),
            &population,
        ));
        assert!(!any_required_ref_out_of_scope(
            "ward_subgraph",
            &None,
            &None,
            &Some(entity("ward:X05001499")),
            &population,
        ));
    }

    // ─── out_of_scope_copy / algorithm_run_failure_copy / algorithm_rail_label ─────────────

    #[test]
    fn out_of_scope_copy_matches_the_copywriting_contract_verbatim() {
        assert_eq!(
            out_of_scope_copy("Jane Smith"),
            "Jane Smith is outside the current graph population and cannot be used in an algorithm."
        );
    }

    #[test]
    fn algorithm_rail_label_maps_every_known_identifier() {
        assert_eq!(algorithm_rail_label("community"), "Community detection");
        assert_eq!(algorithm_rail_label("k_core"), "k-core");
        assert_eq!(
            algorithm_rail_label("betweenness"),
            "Betweenness centrality"
        );
        assert_eq!(algorithm_rail_label("all_simple_paths"), "All simple paths");
        assert_eq!(
            algorithm_rail_label("shortest_paths"),
            "Shortest / k-shortest paths"
        );
        assert_eq!(
            algorithm_rail_label("ward_subgraph"),
            "Ward-reachable subgraph"
        );
    }

    #[test]
    fn algorithm_run_failure_copy_matches_the_d19_extension_format() {
        assert_eq!(
            algorithm_run_failure_copy("all_simple_paths", "timeout waiting for the graph engine"),
            "The All simple paths run failed: timeout waiting for the graph engine."
        );
    }
}
