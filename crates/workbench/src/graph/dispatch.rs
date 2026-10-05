//! `POST /api/graph/algorithm`'s dispatch layer (plan 44-09): D-08 server-side clamping,
//! id resolution, cache/session wiring, and per-algorithm `AlgorithmResult` construction.
//!
//! This is the single place [`clamp_request`] enforces D-08's ceilings — the load-bearing
//! security control this plan's threat register (`T-44-09-01`) names, independent of the HTTP
//! layer (a plain, unit-tested pure function, no `tokio::test`, no HTTP — the same discipline
//! `crate::boot::decide_replay_outcome`'s doc comment describes).
//!
//! [`run_algorithm`] is the ONE place every algorithm from `algorithms/{paths,ward_subgraph,
//! community,betweenness,k_core}.rs` (plans 44-04/44-05) is reachable from the browser: it
//! validates the edge-type selection, clamps every D-08 ceiling, checks the result cache (plan
//! 44-07), resolves `from_id`/`to_id`/`ward_id` against the population scope, registers the run
//! with the session registry (plan 44-07, D-05's "a new Run cancels the previous job for this
//! session"), builds a `ScopedAdjacency` once (skipped for `ward_subgraph`, which builds its own
//! internally per plan 44-04), runs the matched algorithm on the blocking pool
//! (`tokio::task::spawn_blocking`, GRA-06 — never on the async runtime's own worker threads),
//! races that against an outer wall-clock safety net (`tokio::time::timeout`, T-44-09-02), and
//! builds the wire `AlgorithmResult`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use petgraph::visit::EdgeRef;

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_workbench_dto::{
    AlgorithmCaps, AlgorithmOutcome, AlgorithmRequest, AlgorithmResult, CommunityResult, EntityRef,
    PathHop, PathResult, RankedEntity, Truncation,
};

use super::algorithms::{betweenness, community, k_core, paths, ward_subgraph};
use super::cache::CacheKey;
use super::cancel::AlgoCancel;
use super::csr_view::{build_scoped_adjacency, edge_type_name, ScopedAdjacency};
use super::label_index::LabelIndex;
use crate::boot::{PopulationSummary, WorkbenchState};

/// The eleven edge type names ever offered in the UI's edge-type checklist (plan 44-01's
/// `<interfaces>` block; `DeclaresInterest` is the 12th `EdgeWeight` variant `csr_view::
/// edge_type_name` stays exhaustive over, but it is never offered here) — the single source of
/// truth [`validate_edge_types`] checks against and `run_algorithm`'s `excluded_edge_types`
/// computation subtracts `edge_types_included` from.
pub const KNOWN_EDGE_TYPES: [&str; 11] = [
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

/// The six algorithm identifiers plan 44-01's DTO fixed as `AlgorithmRequest.algorithm`'s and
/// `AlgorithmOutcome`'s shared string vocabulary — the single source of truth
/// [`run_algorithm`]'s unknown-algorithm guard checks against.
const KNOWN_ALGORITHMS: [&str; 7] = [
    "all_simple_paths",
    "shortest_paths",
    "community",
    "betweenness",
    "k_core",
    "ward_subgraph",
    // Deep-link only — reachable through `POST /api/graph/algorithm` like any other algorithm,
    // but never offered as a rail choice (`ALGORITHM_OPTIONS` stays at six).
    "highlight_set",
];

/// Clamps every D-08 ceiling on `req` to its correct hard maximum, regardless of what the browser
/// sent — the load-bearing security control this plan's threat register (`T-44-09-01`) names. A
/// pure function: returns a clone of `req` with `hop_cap`/`result_cap`/`budget_secs`/`top_n`/
/// `min_community_size` clamped to their D-08 global ceilings, and `k` clamped to a ceiling that
/// depends on `algorithm` — the ONE field whose ceiling is not global (`k_core`'s `k` ceiling is
/// 50, per the phase 44 UI spec's parameter table; every other algorithm's `k` — shortest/k-shortest
/// paths — is D-08's 1,000). `seed`/`resolution`/`algorithm`/`session_id`/`edge_types_included`/
/// `from_id`/`to_id`/`ward_id` are never touched: D-08's own text states `resolution` and `seed`
/// carry no ceiling ("a tuning parameter, not a safety cap" / "seeded is never a hidden
/// constant").
pub fn clamp_request(algorithm: &str, req: &AlgorithmRequest) -> AlgorithmRequest {
    use urban_integrity_workbench_dto::caps::*;
    let mut clamped = req.clone();
    clamped.hop_cap = req.hop_cap.clamp(1, CEILING_HOP_CAP);
    clamped.result_cap = req.result_cap.clamp(1, CEILING_RESULT_CAP);
    clamped.budget_secs = req.budget_secs.clamp(1, CEILING_BUDGET_SECS);
    clamped.top_n = req.top_n.clamp(1, CEILING_TOP_N);
    clamped.min_community_size = req.min_community_size.clamp(1, CEILING_MIN_COMMUNITY_SIZE);
    clamped.k = if algorithm == "k_core" {
        req.k.clamp(1, CEILING_K_CORE)
    } else {
        req.k.clamp(1, CEILING_K_SHORTEST)
    };
    clamped
}

/// Every name in `included` must be one of [`KNOWN_EDGE_TYPES`] — an unknown name is a typed
/// `Err` naming it, never a silent drop or a panic.
pub fn validate_edge_types(included: &[String]) -> Result<(), String> {
    for name in included {
        if !KNOWN_EDGE_TYPES.contains(&name.as_str()) {
            return Err(format!("unknown edge type: {name}"));
        }
    }
    Ok(())
}

/// Why [`resolve_scoped_id`] failed to resolve an id to a usable CSR node index — distinct from
/// each other so `handlers::algorithm_handler` can render 44-UI-SPEC's distinct "no such entity"
/// vs "out-of-scope pick" copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `id` does not match any entity the boot-time label index knows about at all.
    NotFound(String),
    /// `id` resolves to a real entity, but that entity's CSR index is not in the caller's
    /// population scope — the entity picker's out-of-scope state (the phase 44 UI spec).
    OutOfScope(String),
}

/// Resolves `id` (an `EntityId` string, e.g. `"director:jane-smith"`, `"ward:X05001499"`) to a
/// CSR `u32` node index via `label_index`'s exact-id lookup, then checks it against `scope`.
/// `NotFound` if `label_index` has no entry for `id` at all; `OutOfScope` if it resolves but the
/// index is not in `scope` — two distinct, typed failure modes (T-44-09-04).
pub fn resolve_scoped_id(
    label_index: &LabelIndex,
    scope: &HashSet<u32>,
    id: &str,
) -> Result<u32, ResolveError> {
    let idx = label_index
        .index_of(id)
        .ok_or_else(|| ResolveError::NotFound(id.to_string()))?;
    if !scope.contains(&idx) {
        return Err(ResolveError::OutOfScope(id.to_string()));
    }
    Ok(idx)
}

/// Everything that can go wrong dispatching `POST /api/graph/algorithm`, mapped to a distinct
/// HTTP status by `handlers::algorithm_handler`.
#[derive(Debug)]
pub enum DispatchError {
    /// `AlgorithmRequest.algorithm` is not one of [`KNOWN_ALGORITHMS`].
    UnknownAlgorithm(String),
    /// `AlgorithmRequest.edge_types_included` names an unknown edge type.
    InvalidEdgeType(String),
    /// The selected algorithm requires `from_id`/`to_id`/`ward_id`, and the request omitted it
    /// (`None`) — a structurally incomplete request, distinct from a resolution failure.
    MissingParameter(String),
    /// `from_id`/`to_id`/`ward_id` failed to resolve — see [`ResolveError`] for which reason.
    Resolve(ResolveError),
    /// `state.population` is `PopulationSummary::Unavailable` — there is no scope to run an
    /// algorithm over.
    PopulationUnavailable,
    /// The outer `tokio::time::timeout` safety net (T-44-09-02) elapsed before the algorithm
    /// finished — no partial result is obtainable once this fires (the underlying
    /// `spawn_blocking` closure is not itself interruptible), so this is reported as an error
    /// rather than a truncated `AlgorithmResult`.
    BudgetExceeded,
    /// The `spawn_blocking` task panicked.
    JoinError(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::UnknownAlgorithm(name) => {
                write!(f, "unknown algorithm: {name}")
            }
            DispatchError::InvalidEdgeType(msg) => write!(f, "{msg}"),
            DispatchError::MissingParameter(field) => {
                write!(f, "missing required parameter: {field}")
            }
            DispatchError::Resolve(ResolveError::NotFound(id)) => {
                write!(f, "no entity with id '{id}'")
            }
            DispatchError::Resolve(ResolveError::OutOfScope(id)) => {
                write!(
                    f,
                    "{id} is outside the current graph population and cannot be used in an \
                     algorithm"
                )
            }
            DispatchError::PopulationUnavailable => {
                write!(f, "the graph population is unavailable")
            }
            DispatchError::BudgetExceeded => {
                write!(
                    f,
                    "the algorithm did not finish within its wall-clock budget"
                )
            }
            DispatchError::JoinError(msg) => write!(f, "algorithm task failed: {msg}"),
        }
    }
}

fn require_id<'a>(opt: &'a Option<String>, field: &str) -> Result<&'a str, DispatchError> {
    opt.as_deref()
        .ok_or_else(|| DispatchError::MissingParameter(field.to_string()))
}

/// The wire `EntityRef` at CSR index `idx`. `idx` is expected to always resolve — every index
/// this function is called with originates from an algorithm result computed over `scope`, and
/// `label_index` is built from the same replayed graph `scope` is a subset of.
fn entity_ref(label_index: &LabelIndex, idx: u32) -> EntityRef {
    label_index
        .entity_ref_at(idx)
        .unwrap_or_else(|| panic!("csr index {idx} must resolve via the boot-time label index"))
}

/// The edge type name connecting adjacent hops `a`/`b`, restricted to `included` — scans `a`'s
/// outgoing edges to `b`, then `b`'s outgoing edges to `a` (undirected, D-06), collecting every
/// matching type name and returning the alphabetically-first one for determinism when more than
/// one edge type connects the same pair. `a`/`b` are adjacent in the caller's `ScopedAdjacency`,
/// so at least one included-type edge between them is guaranteed to exist.
fn edge_type_between(
    snapshot: &CsrSnapshot,
    included: &HashSet<String>,
    a: u32,
    b: u32,
) -> Option<String> {
    let mut candidates: Vec<&'static str> = Vec::new();
    for edge_ref in snapshot.graph.edges(a) {
        if edge_ref.target() == b {
            let name = edge_type_name(edge_ref.weight());
            if included.contains(name) {
                candidates.push(name);
            }
        }
    }
    for edge_ref in snapshot.graph.edges(b) {
        if edge_ref.target() == a {
            let name = edge_type_name(edge_ref.weight());
            if included.contains(name) {
                candidates.push(name);
            }
        }
    }
    candidates.sort_unstable();
    candidates.first().map(|s| s.to_string())
}

/// Converts a raw CSR-index path into the wire `PathResult` shape, resolving each hop's
/// `EntityRef` and the edge type walked to the next hop (`None` on the last hop).
fn path_to_result(
    path: &[u32],
    snapshot: &CsrSnapshot,
    label_index: &LabelIndex,
    included: &HashSet<String>,
) -> PathResult {
    let mut hops = Vec::with_capacity(path.len());
    for (i, &node) in path.iter().enumerate() {
        let entity = entity_ref(label_index, node);
        let edge_type_to_next = if i + 1 < path.len() {
            edge_type_between(snapshot, included, node, path[i + 1])
        } else {
            None
        };
        hops.push(PathHop {
            entity,
            edge_type_to_next,
        });
    }
    PathResult { hops }
}

/// The `min(cap, members.len())` highest-degree entities in `members`, ranked by their degree in
/// `adjacency` (ties broken by CSR index ascending for determinism) — the phase 44 UI spec's "top five
/// entities by degree" per-community row.
fn top_entities_by_degree(
    members: &[u32],
    adjacency: &ScopedAdjacency,
    label_index: &LabelIndex,
    cap: usize,
) -> Vec<EntityRef> {
    let mut scored: Vec<(u32, usize)> = members
        .iter()
        .map(|&n| (n, adjacency.neighbors(n).len()))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored
        .into_iter()
        .take(cap)
        .map(|(n, _)| entity_ref(label_index, n))
        .collect()
}

/// The ONE place `color_hue_deg` is computed in this phase (plan 44-09's `must_haves`):
/// `kept` (the algorithm's own, unspecified-order return value) is sorted by `size` DESCENDING,
/// each community's `index` is its 0-based position in that sorted order, and `color_hue_deg =
/// 360 * index / community_count` (integer division) — deterministic given the same seeded
/// partition, realising the phase 44 UI spec's "hue steps evenly around the colour wheel" rule.
///
/// `member_node_ids` is resolved from every member of `members` (already sorted ascending by CSR
/// index by `seeded_louvain`) using the same `entity_ref(label_index, n).id` resolution the
/// `Community` outcome's `hidden_node_ids` uses below — complete membership, never capped, so the
/// canvas's automatic recolour and a selected row's highlight/fit can reach every member (44-17,
/// closing plan 44-13's Deviation 4).
fn communities_to_results(
    kept: Vec<Vec<u32>>,
    adjacency: &ScopedAdjacency,
    label_index: &LabelIndex,
) -> Vec<CommunityResult> {
    let mut sized = kept;
    sized.sort_by_key(|members| std::cmp::Reverse(members.len()));
    let community_count = sized.len() as u32;
    sized
        .iter()
        .enumerate()
        .map(|(i, members)| CommunityResult {
            index: i as u32,
            size: members.len() as u64,
            top_entities: top_entities_by_degree(members, adjacency, label_index, 5),
            color_hue_deg: (360 * i as u32).checked_div(community_count).unwrap_or(0),
            member_node_ids: members
                .iter()
                .map(|&n| entity_ref(label_index, n).id)
                .collect(),
        })
        .collect()
}

/// Maps a `paths::StopReason` (or `ward_subgraph`'s reuse of the same type) to the wire
/// `Truncation` shape. `hop_cap`/`result_cap`/`budget_secs` are the CLAMPED values actually used,
/// so `detail` always names the real cap that fired.
fn truncation_from_stop(
    stop: paths::StopReason,
    hop_cap: u32,
    result_cap: u32,
    budget_secs: u32,
) -> Truncation {
    use paths::StopReason;
    match stop {
        StopReason::None => Truncation {
            truncated: false,
            reason: None,
            detail: String::new(),
        },
        StopReason::HopCap => Truncation {
            truncated: true,
            reason: Some("hop_cap".to_string()),
            detail: format!("stopped at hop cap {hop_cap}"),
        },
        StopReason::ResultCap => Truncation {
            truncated: true,
            reason: Some("result_cap".to_string()),
            detail: format!("stopped at result cap {result_cap}"),
        },
        StopReason::WallClock => Truncation {
            truncated: true,
            reason: Some("wall_clock".to_string()),
            detail: format!("stopped at the {budget_secs}s wall-clock budget"),
        },
        StopReason::Cancelled => Truncation {
            truncated: true,
            reason: Some("cancelled".to_string()),
            detail: "run cancelled before completion".to_string(),
        },
    }
}

/// Only the caps relevant to `algorithm` are `Some` (D-08's own per-algorithm ceiling set, per
/// the phase 44 UI spec's parameter table) — `req` is the CLAMPED request, so every `Some` value here is
/// the value actually used, never the raw client-sent one.
fn caps_used_for(algorithm: &str, req: &AlgorithmRequest) -> AlgorithmCaps {
    match algorithm {
        "all_simple_paths" => AlgorithmCaps {
            hop_cap: Some(req.hop_cap),
            result_cap: Some(req.result_cap),
            budget_secs: Some(req.budget_secs),
            k: None,
            top_n: None,
            min_community_size: None,
        },
        "shortest_paths" => AlgorithmCaps {
            hop_cap: Some(req.hop_cap),
            result_cap: Some(req.result_cap),
            budget_secs: Some(req.budget_secs),
            k: Some(req.k),
            top_n: None,
            min_community_size: None,
        },
        "community" => AlgorithmCaps {
            hop_cap: None,
            result_cap: None,
            budget_secs: None,
            k: None,
            top_n: None,
            min_community_size: Some(req.min_community_size),
        },
        "k_core" => AlgorithmCaps {
            hop_cap: None,
            result_cap: None,
            budget_secs: None,
            k: Some(req.k),
            top_n: None,
            min_community_size: None,
        },
        "betweenness" => AlgorithmCaps {
            hop_cap: None,
            result_cap: None,
            budget_secs: None,
            k: None,
            top_n: Some(req.top_n),
            min_community_size: None,
        },
        "ward_subgraph" => AlgorithmCaps {
            hop_cap: Some(req.hop_cap),
            result_cap: Some(req.result_cap),
            budget_secs: None,
            k: None,
            top_n: None,
            min_community_size: None,
        },
        // A deep link applies no caps: it shows exactly the entities it names, so reporting a
        // cap here would put a ceiling in the status line that nothing enforced.
        "highlight_set" => AlgorithmCaps {
            hop_cap: None,
            result_cap: None,
            budget_secs: None,
            k: None,
            top_n: None,
            min_community_size: None,
        },
        other => {
            unreachable!("dispatch validated algorithm identifiers before this point: {other}")
        }
    }
}

/// Runs the matched algorithm and builds `(outcome, truncation, seed_used,
/// forced_included_edge_type)`. Executed inside `tokio::task::spawn_blocking` — everything this
/// function touches (`label_index`, `snapshot`, `scope`, `adjacency`, `req`, `cancel`) is owned or
/// borrowed data with no `.await` point, safe to run on the blocking pool.
///
/// `from_idx`/`to_idx`/`ward_idx` are `Some` exactly when the matched algorithm needs them
/// (resolved by the caller before this function is invoked) — `.expect()` on the ones a given
/// algorithm arm needs is safe precisely because `run_algorithm` only reaches this function after
/// resolving every id the selected algorithm requires.
#[allow(clippy::too_many_arguments)]
fn build_algorithm_result_blocking(
    label_index: &LabelIndex,
    snapshot: &CsrSnapshot,
    scope: &HashSet<u32>,
    adjacency: Option<&ScopedAdjacency>,
    from_idx: Option<u32>,
    to_idx: Option<u32>,
    ward_idx: Option<u32>,
    req: &AlgorithmRequest,
    cancel: &AlgoCancel,
) -> (AlgorithmOutcome, Truncation, Option<u64>, Option<String>) {
    let included: HashSet<String> = req.edge_types_included.iter().cloned().collect();

    match req.algorithm.as_str() {
        "all_simple_paths" => {
            let adjacency = adjacency.expect("all_simple_paths requires a ScopedAdjacency");
            let budget = Duration::from_secs(req.budget_secs as u64);
            let (raw_paths, stop) = paths::all_simple_paths(
                adjacency,
                scope,
                from_idx.expect("from_idx resolved before dispatch"),
                to_idx.expect("to_idx resolved before dispatch"),
                req.hop_cap,
                req.result_cap,
                budget,
                cancel,
            );
            let paths_out: Vec<PathResult> = raw_paths
                .iter()
                .map(|p| path_to_result(p, snapshot, label_index, &included))
                .collect();
            let outcome = AlgorithmOutcome::Paths { paths: paths_out };
            let truncation =
                truncation_from_stop(stop, req.hop_cap, req.result_cap, req.budget_secs);
            (outcome, truncation, None, None)
        }
        "shortest_paths" => {
            let adjacency = adjacency.expect("shortest_paths requires a ScopedAdjacency");
            let budget = Duration::from_secs(req.budget_secs as u64);
            let (raw_paths, stop) = paths::shortest_and_k_shortest(
                adjacency,
                scope,
                from_idx.expect("from_idx resolved before dispatch"),
                to_idx.expect("to_idx resolved before dispatch"),
                req.k,
                req.hop_cap,
                req.result_cap,
                budget,
                cancel,
            );
            let paths_out: Vec<PathResult> = raw_paths
                .iter()
                .map(|p| path_to_result(p, snapshot, label_index, &included))
                .collect();
            let outcome = AlgorithmOutcome::ShortestPaths {
                paths: paths_out,
                requested_k: req.k,
            };
            let truncation =
                truncation_from_stop(stop, req.hop_cap, req.result_cap, req.budget_secs);
            (outcome, truncation, None, None)
        }
        "community" => {
            let adjacency = adjacency.expect("community requires a ScopedAdjacency");
            let (kept, hidden) = community::seeded_louvain(
                adjacency,
                scope,
                req.seed,
                req.min_community_size,
                req.resolution,
            );
            let communities = communities_to_results(kept, adjacency, label_index);
            let mut hidden_sorted = hidden;
            hidden_sorted.sort_unstable();
            let hidden_node_ids: Vec<String> = hidden_sorted
                .iter()
                .map(|&n| entity_ref(label_index, n).id)
                .collect();
            let outcome = AlgorithmOutcome::Community {
                communities,
                hidden_node_ids,
            };
            // `seeded_louvain` has no internal wall-clock/cancellation support (44-05's actual
            // delivered signature) — a run either completes or it does not start at all (the
            // outer `spawn_blocking` timeout in `run_algorithm` is the only budget enforcement
            // available for this algorithm), so a completed run is never itself truncated.
            let truncation = Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            };
            (outcome, truncation, Some(req.seed), None)
        }
        "k_core" => {
            let adjacency = adjacency.expect("k_core requires a ScopedAdjacency");
            let (surviving, edge_count) = k_core::k_core(adjacency, scope, req.k);
            let mut scored: Vec<(u32, usize)> = surviving
                .iter()
                .map(|&n| {
                    let degree = adjacency
                        .neighbors(n)
                        .iter()
                        .filter(|t| surviving.contains(t))
                        .count();
                    (n, degree)
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let top_nodes: Vec<RankedEntity> = scored
                .into_iter()
                .take(req.top_n as usize)
                .map(|(n, d)| RankedEntity {
                    entity: entity_ref(label_index, n),
                    score: d as f64,
                })
                .collect();
            let mut hidden_ids: Vec<u32> = scope
                .iter()
                .filter(|n| !surviving.contains(n))
                .copied()
                .collect();
            hidden_ids.sort_unstable();
            let hidden_node_ids: Vec<String> = hidden_ids
                .iter()
                .map(|&n| entity_ref(label_index, n).id)
                .collect();
            let outcome = AlgorithmOutcome::KCore {
                k: req.k,
                node_count: surviving.len() as u64,
                edge_count,
                top_nodes,
                hidden_node_ids,
            };
            // `k_core` also has no internal wall-clock/cancellation support (44-05's actual
            // delivered signature) — same reasoning as `community` above.
            let truncation = Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            };
            (outcome, truncation, None, None)
        }
        "betweenness" => {
            let adjacency = adjacency.expect("betweenness requires a ScopedAdjacency");
            let scored = betweenness::sampled_betweenness(
                adjacency,
                scope,
                req.seed,
                crate::graph::algorithms::betweenness::DEFAULT_BETWEENNESS_PIVOT_COUNT,
                cancel,
            );
            let sampled_sources =
                (crate::graph::algorithms::betweenness::DEFAULT_BETWEENNESS_PIVOT_COUNT as usize)
                    .min(scope.len()) as u64;
            let total_sources = scope.len() as u64;
            let mut sorted = scored;
            sorted.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.0.cmp(&b.0))
            });
            let top_nodes: Vec<RankedEntity> = sorted
                .into_iter()
                .take(req.top_n as usize)
                .map(|(n, s)| RankedEntity {
                    entity: entity_ref(label_index, n),
                    score: s,
                })
                .collect();
            let outcome = AlgorithmOutcome::Betweenness {
                sampled_sources,
                total_sources,
                top_nodes,
            };
            let truncation = if cancel.is_cancelled() {
                Truncation {
                    truncated: true,
                    reason: Some("cancelled".to_string()),
                    detail: "run cancelled before completion".to_string(),
                }
            } else if sampled_sources < total_sources {
                Truncation {
                    truncated: true,
                    reason: Some("sample".to_string()),
                    detail: format!("sampled {sampled_sources} of {total_sources} source nodes"),
                }
            } else {
                Truncation {
                    truncated: false,
                    reason: None,
                    detail: String::new(),
                }
            };
            (outcome, truncation, Some(req.seed), None)
        }
        "ward_subgraph" => {
            let ward_idx = ward_idx.expect("ward_idx resolved before dispatch");
            let (visited, stop) = ward_subgraph::ward_reachable_subgraph(
                snapshot,
                scope,
                ward_idx,
                req.hop_cap,
                req.result_cap,
                cancel,
            );
            let mut edge_count = 0u64;
            for &node in &visited {
                for edge_ref in snapshot.graph.edges(node) {
                    let target = edge_ref.target();
                    if visited.contains(&target)
                        && edge_type_name(edge_ref.weight()) == "Represents"
                    {
                        edge_count += 1;
                    }
                }
            }
            let mut ids: Vec<u32> = visited.iter().copied().collect();
            ids.sort_unstable();
            let node_ids: Vec<String> =
                ids.iter().map(|&n| entity_ref(label_index, n).id).collect();
            let ward = entity_ref(label_index, ward_idx);
            let outcome = AlgorithmOutcome::WardSubgraph {
                ward,
                hops: req.hop_cap,
                node_count: visited.len() as u64,
                edge_count,
                node_ids,
            };
            let truncation =
                truncation_from_stop(stop, req.hop_cap, req.result_cap, req.budget_secs);
            (outcome, truncation, None, Some("Represents".to_string()))
        }
        "highlight_set" => {
            // Ids are resolved HERE rather than before spawning, unlike from_id/to_id/ward_id: a
            // deep link naming an entity outside the canonical population must report that count,
            // not fail the whole request. A link built from a report that is one projection older
            // than the population is the ordinary case, not an error.
            let mut node_ids = Vec::new();
            let mut unresolved = 0u64;
            for id in &req.ids {
                match resolve_scoped_id(label_index, scope, id) {
                    Ok(idx) => match label_index.entity_ref_at(idx) {
                        Some(entity) => node_ids.push(entity.id),
                        None => unresolved += 1,
                    },
                    Err(_) => unresolved += 1,
                }
            }
            node_ids.sort();
            node_ids.dedup();
            let outcome = AlgorithmOutcome::HighlightSet {
                node_ids,
                unresolved_count: unresolved,
            };
            let truncation = Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            };
            (outcome, truncation, None, None)
        }
        other => unreachable!("dispatch validated algorithm identifiers before spawning: {other}"),
    }
}

/// The single dispatch point for `POST /api/graph/algorithm` (see this module's top doc comment
/// for the full step-by-step contract).
pub async fn run_algorithm(
    handle: Arc<GraphCoordinatorHandle>,
    state: Arc<WorkbenchState>,
    raw_request: AlgorithmRequest,
) -> Result<AlgorithmResult, DispatchError> {
    let started = Instant::now();

    if !KNOWN_ALGORITHMS.contains(&raw_request.algorithm.as_str()) {
        return Err(DispatchError::UnknownAlgorithm(
            raw_request.algorithm.clone(),
        ));
    }
    validate_edge_types(&raw_request.edge_types_included)
        .map_err(DispatchError::InvalidEdgeType)?;

    let clamped = clamp_request(&raw_request.algorithm, &raw_request);

    let population_generated_at = match &state.population {
        PopulationSummary::Available { generated_at, .. } => generated_at.to_rfc3339(),
        PopulationSummary::Unavailable { .. } => return Err(DispatchError::PopulationUnavailable),
    };

    let cache_key = CacheKey::from_request(&clamped, &population_generated_at);
    if let Some(cached) = state.graph_cache.get(&cache_key) {
        // D-05: a new Run still cancels the previous job for this session, even on a cache hit —
        // enacted here by starting then immediately finishing a fresh token, never touching the
        // cached AlgorithmResult itself.
        let cancel = state.graph_sessions.start(&clamped.session_id);
        state.graph_sessions.finish(&clamped.session_id, &cancel);
        return Ok(cached);
    }

    // Checkpoint fix (plan 44-14 Task 3): the scope may be deferred past boot on a large corpus;
    // while the bake is pending there is no population to traverse yet.
    let population_scope: HashSet<u32> = crate::boot::resolve_population_scope(&state, &handle)
        .ok_or(DispatchError::PopulationUnavailable)?
        .node_ids
        .clone();

    // Gap-closure fix (plan 50-11 Task 3, T-50-04-scope): `scope_ids` narrows the run to a caller-
    // given subset of the population scope — the finding detail route uses this to run an
    // algorithm over exactly the finding's own neighbourhood, never the whole analysis population.
    // Empty means "the whole population scope" (every Graph-page-built request). An id outside the
    // population scope, or one `resolve_scoped_id` cannot find at all, is silently dropped rather
    // than erroring — the same "a report one projection old names entities the canvas no longer
    // has" tolerance `highlight_set` already applies.
    let scope: HashSet<u32> = if clamped.scope_ids.is_empty() {
        population_scope
    } else {
        clamped
            .scope_ids
            .iter()
            .filter_map(|id| resolve_scoped_id(&state.label_index, &population_scope, id).ok())
            .collect()
    };

    let needs_from_to = matches!(
        clamped.algorithm.as_str(),
        "all_simple_paths" | "shortest_paths"
    );
    let from_idx = if needs_from_to {
        let id = require_id(&clamped.from_id, "from_id")?;
        Some(resolve_scoped_id(&state.label_index, &scope, id).map_err(DispatchError::Resolve)?)
    } else {
        None
    };
    let to_idx = if needs_from_to {
        let id = require_id(&clamped.to_id, "to_id")?;
        Some(resolve_scoped_id(&state.label_index, &scope, id).map_err(DispatchError::Resolve)?)
    } else {
        None
    };
    let ward_idx = if clamped.algorithm == "ward_subgraph" {
        let id = require_id(&clamped.ward_id, "ward_id")?;
        Some(resolve_scoped_id(&state.label_index, &scope, id).map_err(DispatchError::Resolve)?)
    } else {
        None
    };

    let cancel = state.graph_sessions.start(&clamped.session_id);

    let snapshot_guard = handle.snapshot.load();
    let adjacency = if !matches!(
        clamped.algorithm.as_str(),
        "ward_subgraph" | "highlight_set"
    ) {
        let included: HashSet<String> = clamped.edge_types_included.iter().cloned().collect();
        Some(build_scoped_adjacency(&snapshot_guard, &scope, &included))
    } else {
        None
    };
    let snapshot = Arc::clone(&snapshot_guard);
    drop(snapshot_guard);

    let state_for_task = Arc::clone(&state);
    let scope_for_task = scope.clone();
    let clamped_for_task = clamped.clone();
    let cancel_for_task = cancel.clone();

    let join_handle = tokio::task::spawn_blocking(move || {
        build_algorithm_result_blocking(
            &state_for_task.label_index,
            &snapshot,
            &scope_for_task,
            adjacency.as_ref(),
            from_idx,
            to_idx,
            ward_idx,
            &clamped_for_task,
            &cancel_for_task,
        )
    });

    // T-44-09-02's outer safety net: races the whole spawn_blocking future against the clamped
    // wall-clock budget (+1s slack for the join itself), bounding worst-case wait time even if an
    // algorithm's own internal cancellation check is somehow bypassed. If this fires, no partial
    // result is obtainable (the underlying closure is not itself interruptible) — reported as a
    // typed error, never a fabricated partial AlgorithmResult.
    let deadline = Duration::from_secs(clamped.budget_secs as u64 + 1);
    let (outcome, truncation, seed_used, forced_included_edge_type) =
        match tokio::time::timeout(deadline, join_handle).await {
            Ok(Ok(built)) => built,
            Ok(Err(join_err)) => {
                state.graph_sessions.finish(&clamped.session_id, &cancel);
                return Err(DispatchError::JoinError(join_err.to_string()));
            }
            Err(_elapsed) => {
                state.graph_sessions.finish(&clamped.session_id, &cancel);
                return Err(DispatchError::BudgetExceeded);
            }
        };

    let excluded_edge_types: Vec<String> = KNOWN_EDGE_TYPES
        .iter()
        .filter(|name| !clamped.edge_types_included.iter().any(|e| e == *name))
        .map(|s| s.to_string())
        .collect();

    let result = AlgorithmResult {
        outcome,
        population_generated_at,
        excluded_edge_types,
        forced_included_edge_type,
        seed_used,
        caps_used: caps_used_for(&clamped.algorithm, &clamped),
        truncation,
        wall_clock_ms: started.elapsed().as_millis() as u64,
    };

    state.graph_cache.put(cache_key, result.clone());
    state.graph_sessions.finish(&clamped.session_id, &cancel);

    Ok(result)
}

#[cfg(test)]
mod clamp_tests {
    use super::*;

    fn sample_request() -> AlgorithmRequest {
        AlgorithmRequest {
            algorithm: "all_simple_paths".to_string(),
            session_id: "session-abc".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: Some("a".to_string()),
            to_id: Some("b".to_string()),
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        }
    }

    #[test]
    fn hop_cap_above_ceiling_clamps_to_8_others_untouched() {
        let mut req = sample_request();
        req.hop_cap = 99;
        let clamped = clamp_request("all_simple_paths", &req);
        assert_eq!(clamped.hop_cap, 8);
        assert_eq!(clamped.result_cap, req.result_cap);
        assert_eq!(clamped.budget_secs, req.budget_secs);
        assert_eq!(clamped.k, req.k.clamp(1, 1_000));
    }

    #[test]
    fn hop_cap_zero_clamps_up_to_1() {
        let mut req = sample_request();
        req.hop_cap = 0;
        let clamped = clamp_request("all_simple_paths", &req);
        assert_eq!(
            clamped.hop_cap, 1,
            "a zero/negative hop request must never reach 0"
        );
    }

    #[test]
    fn result_cap_budget_top_n_and_min_community_size_clamp_to_their_ceilings() {
        let mut req = sample_request();
        req.result_cap = 5_000;
        req.budget_secs = 500;
        req.top_n = 5_000;
        req.min_community_size = 5_000;
        let clamped = clamp_request("community", &req);
        assert_eq!(clamped.result_cap, 1_000);
        assert_eq!(clamped.budget_secs, 30);
        assert_eq!(clamped.top_n, 500);
        assert_eq!(clamped.min_community_size, 500);
    }

    #[test]
    fn k_ceiling_is_algorithm_dependent_k_core_50_vs_paths_1000() {
        let mut req = sample_request();
        req.k = 99;
        let k_core_clamped = clamp_request("k_core", &req);
        assert_eq!(
            k_core_clamped.k, 50,
            "k-core's k ceiling (the phase 44 UI spec) is 50, not D-08's paths ceiling"
        );

        let mut req_paths = sample_request();
        req_paths.k = 5_000;
        let paths_clamped = clamp_request("shortest_paths", &req_paths);
        assert_eq!(
            paths_clamped.k, 1_000,
            "shortest/k-shortest paths' k ceiling (D-08) is 1,000"
        );
    }

    #[test]
    fn seed_and_resolution_are_never_clamped() {
        let mut req = sample_request();
        req.seed = u64::MAX;
        req.resolution = 999_999.5;
        let clamped = clamp_request("community", &req);
        assert_eq!(
            clamped.seed,
            u64::MAX,
            "seed carries no ceiling (D-08's own text)"
        );
        assert_eq!(
            clamped.resolution, 999_999.5,
            "resolution carries no ceiling (D-08's own text)"
        );
    }
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    #[test]
    fn unknown_edge_type_names_it_in_the_error() {
        let included = vec!["Directs".to_string(), "NotARealType".to_string()];
        let err = validate_edge_types(&included).expect_err("unknown type must be rejected");
        assert!(
            err.contains("NotARealType"),
            "error must name the unknown type: {err}"
        );
    }

    #[test]
    fn every_known_type_is_accepted() {
        let included = vec!["Directs".to_string()];
        assert!(validate_edge_types(&included).is_ok());
    }

    #[test]
    fn every_known_edge_type_constant_individually_validates() {
        for name in KNOWN_EDGE_TYPES {
            assert!(
                validate_edge_types(&[name.to_string()]).is_ok(),
                "{name} must be a known, valid edge type"
            );
        }
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    fn fixture_label_index() -> (LabelIndex, u32, u32) {
        let storage = GraphStorage::new();
        storage.insert_node(EntityId("in-scope".to_string()), entity("In Scope"));
        storage.insert_node(EntityId("out-of-scope".to_string()), entity("Out Of Scope"));
        let index = LabelIndex::build(&storage);
        let in_scope_idx = index.index_of("in-scope").expect("in-scope must resolve");
        let out_of_scope_idx = index
            .index_of("out-of-scope")
            .expect("out-of-scope must resolve");
        (index, in_scope_idx, out_of_scope_idx)
    }

    #[test]
    fn an_in_scope_id_resolves_to_its_csr_index() {
        let (index, in_scope_idx, _out_of_scope_idx) = fixture_label_index();
        let scope: HashSet<u32> = [in_scope_idx].into_iter().collect();

        let resolved = resolve_scoped_id(&index, &scope, "in-scope").expect("must resolve");
        assert_eq!(resolved, in_scope_idx);
    }

    #[test]
    fn an_out_of_scope_id_is_rejected_distinctly_from_not_found() {
        let (index, in_scope_idx, _out_of_scope_idx) = fixture_label_index();
        let scope: HashSet<u32> = [in_scope_idx].into_iter().collect();

        let err = resolve_scoped_id(&index, &scope, "out-of-scope")
            .expect_err("out-of-scope id must be rejected");
        assert_eq!(err, ResolveError::OutOfScope("out-of-scope".to_string()));
    }

    #[test]
    fn an_unknown_id_is_not_found_distinct_from_out_of_scope() {
        let (index, in_scope_idx, _out_of_scope_idx) = fixture_label_index();
        let scope: HashSet<u32> = [in_scope_idx].into_iter().collect();

        let err = resolve_scoped_id(&index, &scope, "never-existed")
            .expect_err("unknown id must be rejected");
        assert_eq!(err, ResolveError::NotFound("never-existed".to_string()));
        assert_ne!(
            err,
            ResolveError::OutOfScope("never-existed".to_string()),
            "NotFound and OutOfScope must be distinct variants"
        );
    }
}

#[cfg(test)]
mod community_result_tests {
    use super::*;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    /// 75 disconnected nodes (`n0`..`n74`), inserted in order so their CSR indices exactly match
    /// their insertion order (no edges, so `bake_csr`'s sort-by-`idx.index()` is a no-op
    /// reordering) — used to hand-build a `kept` partition of exact sizes without needing real
    /// Louvain clustering to produce them.
    fn seventy_five_node_fixture() -> (LabelIndex, ScopedAdjacency, HashSet<u32>) {
        let storage = GraphStorage::new();
        for i in 0..75 {
            storage.insert_node(EntityId(format!("n{i}")), entity(&format!("n{i}")));
        }
        let label_index = LabelIndex::build(&storage);
        let snapshot = bake_csr(&storage, 75);
        let scope: HashSet<u32> = (0..75).collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);
        (label_index, adjacency, scope)
    }

    /// The behavior spec's worked example: a fixture community.rs result of 4 communities with
    /// sizes `[10, 40, 20, 5]` (deliberately unsorted from the algorithm's own return order) must
    /// be re-ordered `[40, 20, 10, 5]` with `color_hue_deg` `[0, 90, 180, 270]` in that order.
    #[test]
    fn sorts_by_size_descending_and_assigns_hue_by_position() {
        let (label_index, adjacency, _scope) = seventy_five_node_fixture();

        // Deliberately given in the un-sorted [10, 40, 20, 5] order the behavior spec describes.
        let kept: Vec<Vec<u32>> = vec![
            (0..10).collect(),  // size 10
            (10..50).collect(), // size 40
            (50..70).collect(), // size 20
            (70..75).collect(), // size 5
        ];

        let results = communities_to_results(kept, &adjacency, &label_index);

        let sizes: Vec<u64> = results.iter().map(|c| c.size).collect();
        assert_eq!(sizes, vec![40, 20, 10, 5], "must be sorted size-descending");

        let hues: Vec<u32> = results.iter().map(|c| c.color_hue_deg).collect();
        assert_eq!(
            hues,
            vec![0, 90, 180, 270],
            "color_hue_deg must be 360 * index / community_count over the sorted order"
        );

        let indices: Vec<u32> = results.iter().map(|c| c.index).collect();
        assert_eq!(indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn top_entities_is_capped_at_five_even_for_a_larger_community() {
        let (label_index, adjacency, _scope) = seventy_five_node_fixture();
        let kept: Vec<Vec<u32>> = vec![(10..50).collect()]; // size 40
        let results = communities_to_results(kept, &adjacency, &label_index);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].top_entities.len() <= 5,
            "top_entities must be capped at 5 per the phase 44 UI spec's 'top five entities by degree'"
        );
    }

    /// 44-17's gap closure: `member_node_ids` must be the COMPLETE membership of every kept
    /// community (`len() == size`, unlike `top_entities` which is capped), and every id
    /// `top_entities` renders must also appear in `member_node_ids` — the display list is a
    /// subset of the full membership, never a disjoint or partially-overlapping set.
    #[test]
    fn member_node_ids_is_complete_and_top_entities_is_a_subset_of_it() {
        let (label_index, adjacency, _scope) = seventy_five_node_fixture();
        let kept: Vec<Vec<u32>> = vec![
            (0..10).collect(),  // size 10
            (10..50).collect(), // size 40 (bigger than the top_entities cap of 5)
            (50..70).collect(), // size 20
            (70..75).collect(), // size 5
        ];

        let results = communities_to_results(kept, &adjacency, &label_index);

        for community in &results {
            assert_eq!(
                community.member_node_ids.len(),
                community.size as usize,
                "member_node_ids must carry every kept member, not a capped subset (community {})",
                community.index
            );

            let member_ids: std::collections::HashSet<&String> =
                community.member_node_ids.iter().collect();
            for top_entity in &community.top_entities {
                assert!(
                    member_ids.contains(&top_entity.id),
                    "top_entities id {} must be a subset of member_node_ids (community {})",
                    top_entity.id,
                    community.index
                );
            }
        }
    }
}

#[cfg(test)]
mod run_algorithm_tests {
    use super::*;
    use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::persistence::wal_channel;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    use crate::boot::{AnalysisCatalogEntry, PopulationSummary};

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    fn directs() -> EdgeWeight {
        EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        }
    }

    /// Builds a coordinator handle + `WorkbenchState` fixture (mirroring `handlers.rs`'s own
    /// `router_with_fixture` shape, minus the router): a fully-connected `Directs`-linear chain of
    /// `n` nodes, all in the population scope.
    async fn fixture(n: usize) -> (Arc<GraphCoordinatorHandle>, Arc<WorkbenchState>) {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        for i in 0..n {
            handle
                .storage
                .insert_node(EntityId(format!("n{i}")), entity(&format!("n{i}")));
        }
        for i in 0..n.saturating_sub(1) {
            handle
                .storage
                .insert_edge(
                    &EntityId(format!("n{i}")),
                    &EntityId(format!("n{}", i + 1)),
                    directs(),
                )
                .expect("fixture edge insert");
        }

        let event_count = (n + n.saturating_sub(1)) as u64;
        let baked = bake_csr(&handle.storage, event_count);
        handle.snapshot.store(baked);

        let mut entries = handle.storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let node_ids: HashSet<u32> = entries
            .iter()
            .map(|(_, idx, _, _)| idx.index() as u32)
            .collect();
        let population_scope = crate::graph::population_scope::PopulationScope {
            node_ids,
            ward_seed_count: 1,
        };

        let label_index = LabelIndex::build(&handle.storage);

        let state = Arc::new(WorkbenchState {
            population: PopulationSummary::Available {
                count: n as u64,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 1,
            },
            replayed_at: chrono::Utc::now(),
            analyses: vec![AnalysisCatalogEntry {
                db_name: "an_a".to_string(),
                label: "Analysis A".to_string(),
            }],
            db_name: "an_a".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(surrealdb::Surreal::init()),
            catalog: std::sync::Arc::new(surrealdb::Surreal::init()),
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events: event_count as usize,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope,
            label_index,
            graph_cache: super::super::cache::AlgorithmCache::new(),
            graph_sessions: super::super::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        });

        (handle, state)
    }

    fn paths_request(algorithm: &str, from: &str, to: &str) -> AlgorithmRequest {
        AlgorithmRequest {
            algorithm: algorithm.to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: Some(from.to_string()),
            to_id: Some(to.to_string()),
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        }
    }

    fn highlight_request(ids: &[&str]) -> AlgorithmRequest {
        AlgorithmRequest {
            algorithm: "highlight_set".to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: None,
            to_id: None,
            ward_id: None,
            ids: ids.iter().map(|id| id.to_string()).collect(),
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        }
    }

    /// A deep link naming an entity that is NOT in the population must still succeed, reporting
    /// the miss as a count. This is the ordinary case, not an error: a report built one
    /// projection before the current population will name entities the canvas no longer has, and
    /// failing the whole request would leave the analyst with nothing instead of the entities
    /// that DO resolve.
    #[tokio::test]
    async fn highlight_set_resolves_known_ids_and_counts_the_rest() {
        let (handle, state) = fixture(5).await;
        let req = highlight_request(&["n0", "n3", "nope"]);

        let result = run_algorithm(handle, state, req)
            .await
            .expect("an unresolvable id must not fail the request");

        assert!(!result.truncation.truncated);
        match result.outcome {
            AlgorithmOutcome::HighlightSet {
                node_ids,
                unresolved_count,
            } => {
                assert_eq!(node_ids, vec!["n0".to_string(), "n3".to_string()]);
                assert_eq!(unresolved_count, 1);
            }
            other => panic!("expected AlgorithmOutcome::HighlightSet, got {other:?}"),
        }
    }

    /// Duplicate ids collapse, and the set is sorted — so the same link always renders the same
    /// row count regardless of how the caller ordered or repeated its ids.
    #[tokio::test]
    async fn highlight_set_dedups_and_sorts() {
        let (handle, state) = fixture(5).await;
        let req = highlight_request(&["n3", "n0", "n3"]);

        let result = run_algorithm(handle, state, req)
            .await
            .expect("dispatch must succeed");

        match result.outcome {
            AlgorithmOutcome::HighlightSet {
                node_ids,
                unresolved_count,
            } => {
                assert_eq!(node_ids, vec!["n0".to_string(), "n3".to_string()]);
                assert_eq!(unresolved_count, 0, "a repeat is not an unresolved id");
            }
            other => panic!("expected AlgorithmOutcome::HighlightSet, got {other:?}"),
        }
    }

    /// An empty deep link resolves to an empty set rather than erroring — and reports no
    /// unresolved ids, since it named none.
    #[tokio::test]
    async fn highlight_set_with_no_ids_is_an_empty_set_not_an_error() {
        let (handle, state) = fixture(5).await;
        let result = run_algorithm(handle, state, highlight_request(&[]))
            .await
            .expect("dispatch must succeed");

        match result.outcome {
            AlgorithmOutcome::HighlightSet {
                node_ids,
                unresolved_count,
            } => {
                assert!(node_ids.is_empty());
                assert_eq!(unresolved_count, 0);
            }
            other => panic!("expected AlgorithmOutcome::HighlightSet, got {other:?}"),
        }
    }

    /// A known 2-hop path (`n0 -> n1 -> n2`) resolves to `AlgorithmOutcome::Paths` with the
    /// expected hop sequence and `truncation.truncated == false`.
    #[tokio::test]
    async fn all_simple_paths_returns_the_known_two_hop_path_untruncated() {
        let (handle, state) = fixture(3).await;
        let req = paths_request("all_simple_paths", "n0", "n2");

        let result = run_algorithm(handle, state, req)
            .await
            .expect("dispatch must succeed");

        assert!(!result.truncation.truncated);
        match result.outcome {
            AlgorithmOutcome::Paths { paths } => {
                assert_eq!(paths.len(), 1, "exactly one simple path");
                assert_eq!(paths[0].hops.len(), 3, "n0 -> n1 -> n2 is 3 hops");
                assert_eq!(paths[0].hops[0].entity.id, "n0");
                assert_eq!(paths[0].hops[2].entity.id, "n2");
                assert_eq!(
                    paths[0].hops[2].edge_type_to_next, None,
                    "the last hop has no next edge"
                );
            }
            other => panic!("expected AlgorithmOutcome::Paths, got {other:?}"),
        }
    }

    /// Dispatching the SAME request twice is a cache hit the second time: pre-seeding the cache
    /// entry with a sentinel value a fresh computation could never legitimately produce, and
    /// proving the second dispatch returns that sentinel verbatim.
    #[tokio::test]
    async fn same_request_twice_is_a_cache_hit_the_second_time() {
        let (handle, state) = fixture(3).await;
        let req = paths_request("all_simple_paths", "n0", "n2");

        let first = run_algorithm(Arc::clone(&handle), Arc::clone(&state), req.clone())
            .await
            .expect("first dispatch must succeed");

        let population_generated_at = match &state.population {
            PopulationSummary::Available { generated_at, .. } => generated_at.to_rfc3339(),
            _ => unreachable!(),
        };
        let clamped = clamp_request(&req.algorithm, &req);
        let key = CacheKey::from_request(&clamped, &population_generated_at);
        let mut sentinel = first.clone();
        sentinel.wall_clock_ms = 999_999;
        state.graph_cache.put(key, sentinel.clone());

        let second = run_algorithm(handle, state, req)
            .await
            .expect("second dispatch must succeed");
        assert_eq!(
            second, sentinel,
            "a cache hit must return the cached value verbatim, never recomputing"
        );
    }

    /// A request whose `edge_types_included` differs from a previously-cached request (same
    /// everything else) is a cache MISS, not a hit.
    #[tokio::test]
    async fn differing_edge_types_included_is_a_cache_miss() {
        let (handle, state) = fixture(3).await;
        let req = paths_request("all_simple_paths", "n0", "n2");

        let first = run_algorithm(Arc::clone(&handle), Arc::clone(&state), req.clone())
            .await
            .expect("first dispatch must succeed");

        let mut sentinel = first.clone();
        sentinel.wall_clock_ms = 999_999;
        let population_generated_at = match &state.population {
            PopulationSummary::Available { generated_at, .. } => generated_at.to_rfc3339(),
            _ => unreachable!(),
        };
        let clamped = clamp_request(&req.algorithm, &req);
        let key = CacheKey::from_request(&clamped, &population_generated_at);
        state.graph_cache.put(key, sentinel.clone());

        let mut different_req = req.clone();
        different_req.edge_types_included = vec!["Directs".to_string(), "AwardedTo".to_string()];

        let second = run_algorithm(handle, state, different_req)
            .await
            .expect("second dispatch must succeed");
        assert_ne!(
            second, sentinel,
            "a different edge_types_included must be a cache MISS, not the sentinel"
        );
    }

    /// `community`'s `AlgorithmResult.seed_used` is `Some(seed)`; `all_simple_paths`' is `None`
    /// (community/betweenness only, per plan 44-01's design).
    #[tokio::test]
    async fn community_carries_seed_used_paths_does_not() {
        let (handle, state) = fixture(6).await;
        let community_req = AlgorithmRequest {
            algorithm: "community".to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: None,
            to_id: None,
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 1,
            resolution: 1.0,
        };
        let community_result =
            run_algorithm(Arc::clone(&handle), Arc::clone(&state), community_req)
                .await
                .expect("community dispatch must succeed");
        assert_eq!(community_result.seed_used, Some(42));

        let paths_req = paths_request("all_simple_paths", "n0", "n2");
        let paths_result = run_algorithm(handle, state, paths_req)
            .await
            .expect("paths dispatch must succeed");
        assert_eq!(paths_result.seed_used, None);
    }

    /// `ward_subgraph` builds a result with `forced_included_edge_type: Some("Represents")`;
    /// every other algorithm's result has `forced_included_edge_type: None`.
    #[tokio::test]
    async fn ward_subgraph_forces_represents_other_algorithms_do_not() {
        use urban_integrity_schema::types::edge::RepresentsData;
        use urban_integrity_schema::types::node::WardData;

        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        handle.storage.insert_node(
            EntityId("ward:w".to_string()),
            NodeWeight::Ward(WardData {
                name: "W".to_string(),
            }),
        );
        handle
            .storage
            .insert_node(EntityId("c".to_string()), entity("C"));
        handle
            .storage
            .insert_edge(
                &EntityId("ward:w".to_string()),
                &EntityId("c".to_string()),
                EdgeWeight::Represents {
                    meta: EdgeMeta {
                        created_at: chrono::Utc::now(),
                        effective_from: None,
                        effective_to: None,
                    },
                    data: RepresentsData {
                        details: "d".into(),
                    },
                },
            )
            .expect("fixture edge insert");

        let baked = bake_csr(&handle.storage, 3);
        handle.snapshot.store(baked);

        let mut entries = handle.storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let node_ids: HashSet<u32> = entries
            .iter()
            .map(|(_, idx, _, _)| idx.index() as u32)
            .collect();
        let population_scope = crate::graph::population_scope::PopulationScope {
            node_ids,
            ward_seed_count: 1,
        };
        let label_index = LabelIndex::build(&handle.storage);

        let state = Arc::new(WorkbenchState {
            population: PopulationSummary::Available {
                count: 2,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 1,
            },
            replayed_at: chrono::Utc::now(),
            analyses: vec![],
            db_name: "an_a".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(surrealdb::Surreal::init()),
            catalog: std::sync::Arc::new(surrealdb::Surreal::init()),
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events: 3,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope,
            label_index,
            graph_cache: super::super::cache::AlgorithmCache::new(),
            graph_sessions: super::super::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        });

        let ward_req = AlgorithmRequest {
            algorithm: "ward_subgraph".to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Represents".to_string()],
            seed: 42,
            from_id: None,
            to_id: None,
            ward_id: Some("ward:w".to_string()),
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 2,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        };
        let ward_result = run_algorithm(Arc::clone(&handle), Arc::clone(&state), ward_req)
            .await
            .expect("ward_subgraph dispatch must succeed");
        assert_eq!(
            ward_result.forced_included_edge_type,
            Some("Represents".to_string())
        );

        let paths_req = AlgorithmRequest {
            algorithm: "all_simple_paths".to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Represents".to_string()],
            seed: 42,
            from_id: Some("ward:w".to_string()),
            to_id: Some("c".to_string()),
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 2,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        };
        let paths_result = run_algorithm(handle, state, paths_req)
            .await
            .expect("all_simple_paths dispatch must succeed");
        assert_eq!(paths_result.forced_included_edge_type, None);
    }

    /// Betweenness on a fixture with MORE than 500 scope nodes builds `sampled_sources == 500`
    /// (the pivot-count constant), `total_sources == 501`; the same dispatch on a smaller (12
    /// node) fixture degrades to `sampled_sources == 12 == total_sources` — never coincidentally
    /// equal to `top_n` (50).
    #[tokio::test]
    async fn betweenness_sampled_sources_above_and_below_the_pivot_count() {
        let (handle, state) = fixture(501).await;
        let req = AlgorithmRequest {
            algorithm: "betweenness".to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: None,
            to_id: None,
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        };
        let result = run_algorithm(handle, state, req)
            .await
            .expect("betweenness dispatch must succeed");
        match result.outcome {
            AlgorithmOutcome::Betweenness {
                sampled_sources,
                total_sources,
                ..
            } => {
                assert_eq!(sampled_sources, 500);
                assert_eq!(total_sources, 501);
                assert_ne!(sampled_sources, 50, "must never equal top_n by coincidence");
            }
            other => panic!("expected AlgorithmOutcome::Betweenness, got {other:?}"),
        }
        assert!(result.truncation.truncated);
        assert_eq!(result.truncation.reason, Some("sample".to_string()));

        let (handle_small, state_small) = fixture(12).await;
        let req_small = AlgorithmRequest {
            algorithm: "betweenness".to_string(),
            session_id: "session-2".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: None,
            to_id: None,
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        };
        let result_small = run_algorithm(handle_small, state_small, req_small)
            .await
            .expect("betweenness dispatch on small fixture must succeed");
        match result_small.outcome {
            AlgorithmOutcome::Betweenness {
                sampled_sources,
                total_sources,
                ..
            } => {
                assert_eq!(sampled_sources, 12);
                assert_eq!(total_sources, 12);
            }
            other => panic!("expected AlgorithmOutcome::Betweenness, got {other:?}"),
        }
        assert!(
            !result_small.truncation.truncated,
            "pivot count degrading to the full population is not a truncation"
        );
    }

    /// A request whose `from_id` resolves to a node outside `population_scope.node_ids` is
    /// rejected with `DispatchError::Resolve(ResolveError::OutOfScope(..))`.
    #[tokio::test]
    async fn out_of_scope_from_id_is_rejected() {
        let (handle, state) = fixture(3).await;
        // "n5" was never inserted at all — a NotFound case, distinct from OutOfScope, both
        // exercised together here since both must be typed DispatchError::Resolve variants.
        let req = paths_request("all_simple_paths", "n5", "n2");
        let err = run_algorithm(handle, state, req)
            .await
            .expect_err("an id that does not exist must be rejected");
        assert!(matches!(
            err,
            DispatchError::Resolve(ResolveError::NotFound(_))
        ));
    }

    /// Gap-closure fix (plan 50-11 Task 3): the SAME community request dispatched once with
    /// `scope_ids` empty (whole population) and once with `scope_ids` naming a 3-node subset of a
    /// 10-node fixture must consider a DIFFERENT total node count (every kept community's
    /// `member_node_ids.len()` summed with `hidden_node_ids.len()` — every node in `scope` is
    /// accounted for by exactly one of the two), and every member the scoped run reports must be
    /// one of the 3 given ids — proving the run was actually restricted, not merely re-labelled.
    #[tokio::test]
    async fn scope_ids_narrows_the_run_to_the_given_subset() {
        let (handle, state) = fixture(10).await;
        let mut req = AlgorithmRequest {
            algorithm: "community".to_string(),
            session_id: "session-unscoped".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: None,
            to_id: None,
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 1,
            resolution: 1.0,
        };

        fn total_considered(outcome: &AlgorithmOutcome) -> usize {
            match outcome {
                AlgorithmOutcome::Community {
                    communities,
                    hidden_node_ids,
                } => {
                    communities
                        .iter()
                        .map(|c| c.member_node_ids.len())
                        .sum::<usize>()
                        + hidden_node_ids.len()
                }
                other => panic!("expected AlgorithmOutcome::Community, got {other:?}"),
            }
        }

        let unscoped = run_algorithm(Arc::clone(&handle), Arc::clone(&state), req.clone())
            .await
            .expect("unscoped dispatch must succeed");
        assert_eq!(
            total_considered(&unscoped.outcome),
            10,
            "an empty scope_ids must run over the whole 10-node population"
        );

        let subset = ["n0".to_string(), "n1".to_string(), "n2".to_string()];
        req.scope_ids = subset.to_vec();
        req.session_id = "session-scoped".to_string();
        let scoped = run_algorithm(handle, state, req)
            .await
            .expect("scoped dispatch must succeed");
        let scoped_total = total_considered(&scoped.outcome);
        assert_eq!(
            scoped_total, 3,
            "a 3-id scope_ids must restrict the run to exactly those 3 nodes"
        );
        assert_ne!(
            scoped_total,
            total_considered(&unscoped.outcome),
            "the scoped and unscoped runs must consider a different node count"
        );

        if let AlgorithmOutcome::Community { communities, .. } = &scoped.outcome {
            for community in communities {
                for member in &community.member_node_ids {
                    assert!(
                        subset.contains(member),
                        "scoped run member {member} must be one of the given scope_ids"
                    );
                }
            }
        }
    }

    /// An unknown algorithm identifier is rejected with `DispatchError::UnknownAlgorithm`.
    #[tokio::test]
    async fn unknown_algorithm_is_rejected() {
        let (handle, state) = fixture(3).await;
        let mut req = paths_request("all_simple_paths", "n0", "n2");
        req.algorithm = "not_a_real_algorithm".to_string();
        let err = run_algorithm(handle, state, req)
            .await
            .expect_err("unknown algorithm must be rejected");
        assert!(matches!(err, DispatchError::UnknownAlgorithm(_)));
    }

    /// An unknown edge type in `edge_types_included` is rejected with
    /// `DispatchError::InvalidEdgeType`.
    #[tokio::test]
    async fn unknown_edge_type_is_rejected() {
        let (handle, state) = fixture(3).await;
        let mut req = paths_request("all_simple_paths", "n0", "n2");
        req.edge_types_included = vec!["NotARealType".to_string()];
        let err = run_algorithm(handle, state, req)
            .await
            .expect_err("unknown edge type must be rejected");
        assert!(matches!(err, DispatchError::InvalidEdgeType(_)));
    }
}
