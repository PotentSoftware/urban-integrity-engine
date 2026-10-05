//! `GET /api/entity/neighbourhood` — CNV-01's "expand N hops at a time", with the node cap
//! reported every time it bites (D-12).
//!
//! # Where the nodes come from, and why not from the CSR snapshot
//!
//! The walk is `crates/core::persistence::queries::query_entity_neighbourhood` — the read path
//! 45-02 settled the definition of a neighbour in (45-CONTEXT open question 3) — **not** the
//! in-process `CsrSnapshot` the phase-44 canvas algorithms traverse. The snapshot is faster and it
//! was the obvious shortcut, and it answers a different question: it is replayed from
//! `graph_events` alone, so it carries none of the `identity_links` that the settled definition
//! includes and labels `IdentityLink`. A neighbourhood taken from it would silently omit exactly
//! the neighbours `graph-path` reaches in one hop, and nothing would fail.
//!
//! The cost of that choice is honest and worth stating: this route pages the whole `EdgeAdded`
//! table plus the identity links on every request, the same load `persistence::traversal::
//! bfs_traversal` pays for `graph-path`. One index load serves the whole expansion, so `hops = 3`
//! costs no more reads than `hops = 1` — but two consecutive expansions cost it twice. If that
//! becomes the thing the operator waits on, the fix is a generation-keyed cache of
//! `NeighbourIndex` on `WorkbenchState` (the shape `AlgorithmCache` already uses), not a second
//! neighbour definition.
//!
//! # Caps
//!
//! Both come from `workbench-dto::caps` (D-13) and are clamped server-side: a browser asking for
//! 40 hops gets `CEILING_NEIGHBOURHOOD_HOPS`. The *applied* values travel back on the response, so
//! the status line states what the server did rather than what the client asked for.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::Query;
use axum::response::Json;
use axum::Extension;

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_core::persistence::queries::{
    effective_range, query_entity_neighbourhood_multi,
};
use urban_integrity_workbench_dto::caps::{
    CEILING_NEIGHBOURHOOD_HOPS, CEILING_NEIGHBOURHOOD_NODE_CAP, CEILING_NEIGHBOURHOOD_SEEDS,
    DEFAULT_NEIGHBOURHOOD_HOPS, DEFAULT_NEIGHBOURHOOD_NODE_CAP,
};
use urban_integrity_workbench_dto::neighbourhood::{
    NeighbourEdgeView, NeighbourhoodNodeView, NeighbourhoodView,
};
use urban_integrity_workbench_dto::temporal::Temporal;

use crate::boot::{resolve_hub_ids, WorkbenchState};
use crate::entity::profile_map::entity_ref_for;
use crate::entity::{bad_request, internal, ApiError};

/// Query parameters for [`neighbourhood_handler`]. `id`/`ids` together select the seed set,
/// `hops`/`node_cap` select *within* the fixed analysis.
#[derive(Debug, Default, serde::Deserialize)]
pub struct NeighbourhoodQuery {
    id: Option<String>,
    /// A CSV of additional seed ids (Phase 50, D-12) — a finding's own `RecordRef`s, seeding the
    /// same walk `id` seeds alone. Both may be present at once; see [`collect_seed_ids`].
    ids: Option<String>,
    hops: Option<u32>,
    node_cap: Option<u32>,
}

/// `id`/`to`/`ids` -> a deduplicated, first-seen-order seed list.
///
/// A native, independently-maintained duplicate of
/// `crates/workbench-web/src/graph_deep_link.rs::collect_ids`'s exact trim/dedupe/first-seen-order
/// logic — `workbench-web` is a wasm-only crate this native one cannot depend on, the same
/// boundary `tool_rail.rs`'s `ALL_EDGE_TYPES` doc comment names as this repo's established
/// precedent for a deliberate duplicate rather than a shared dependency. Kept at the same
/// three-slot `(from, to, ids)` signature as the original even though this call site only ever
/// fills `from` and `ids`, so a future diff between the two copies stays easy to read.
///
/// The one deliberate divergence from the browser copy is `cap` (T-50-01, WR-04): collection
/// stops at `cap` distinct ids, so the server never walks — or even deduplicates — more seeds
/// than [`CEILING_NEIGHBOURHOOD_SEEDS`], whatever the request carried. Stopping early rather than
/// truncating afterwards matters: the dedupe is a linear scan per id, so an unbounded `ids=` list
/// would otherwise cost quadratic time before the cap was ever applied.
fn collect_seed_ids(
    from: Option<&str>,
    to: Option<&str>,
    ids: Option<&str>,
    cap: usize,
) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    let csv_values = ids.into_iter().flat_map(|csv| csv.split(','));
    for value in [from, to].into_iter().flatten().chain(csv_values) {
        if list.len() >= cap {
            break;
        }
        let value = value.trim();
        if !value.is_empty() && !list.iter().any(|existing| existing == value) {
            list.push(value.to_string());
        }
    }
    list
}

/// `GET /api/entity/neighbourhood?id=…&ids=…&hops=…&node_cap=…`.
///
/// An unknown id is **not** a 404 here: a node with no edges and a node that does not exist both
/// have an empty neighbourhood, and the profile route is where "no such entity" is established.
/// Returning 200 with an empty, honestly-capped neighbourhood keeps the canvas's behaviour the
/// same for an isolated node as for a missing one, which is what it can actually draw.
///
/// `ids` (Phase 50, D-12) names additional seeds beyond `id` — either or both may be present, and
/// every named seed is walked from at once, unioned by nearest hop. A hub node
/// ([`resolve_hub_ids`]) reached during the walk is included in the result but is never used to
/// expand the walk further (T-50-02) — enforced inside
/// [`urban_integrity_core::persistence::queries::query_entity_neighbourhood_multi`]'s own
/// traversal, not only reported afterward through the node cap.
pub async fn neighbourhood_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<NeighbourhoodQuery>,
) -> Result<Json<NeighbourhoodView>, ApiError> {
    // Bounded server-side (T-50-01, WR-04), never left to the browser's own `ids=` cap: the
    // seeds actually walked are what `NeighbourhoodView::seeds` reports back.
    let seed_ids = collect_seed_ids(
        params.id.as_deref(),
        None,
        params.ids.as_deref(),
        CEILING_NEIGHBOURHOOD_SEEDS,
    );
    if seed_ids.is_empty() {
        return Err(bad_request("`id` or `ids` is required"));
    }

    // Clamped, never trusted — the ceiling is the server's (T-44-09-01's discipline, D-13's
    // single source of truth). A zero hop budget would ask for the seeds on their own, which is
    // not a neighbourhood; a zero cap would return nothing however far the walk reached.
    let hops = params
        .hops
        .unwrap_or(DEFAULT_NEIGHBOURHOOD_HOPS)
        .clamp(1, CEILING_NEIGHBOURHOOD_HOPS);
    let node_cap = params
        .node_cap
        .unwrap_or(DEFAULT_NEIGHBOURHOOD_NODE_CAP)
        .clamp(1, CEILING_NEIGHBOURHOOD_NODE_CAP);

    // The hub set is measured over the resolved population scope; while the first CSR bake is
    // still pending there is no scope to measure it over, and walking without it would silently
    // drop T-50-02's hub exclusion — so say "not yet", the same 503 every other
    // population-scoped route gives (WR-03).
    let Some(hub_ids) = resolve_hub_ids(&handle, &state) else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "population not yet available: the graph snapshot is still baking — retry shortly"
            })),
        ));
    };
    let hub_ids: &HashSet<String> = hub_ids;

    let discovered = query_entity_neighbourhood_multi(&state.db, &seed_ids, hops, hub_ids)
        .await
        .map_err(|e| internal(format!("neighbourhood read failed: {e}")))?;

    let nodes: Vec<NeighbourhoodNodeView> = discovered
        .into_iter()
        .map(|n| NeighbourhoodNodeView {
            entity: entity_ref_for(&state.label_index, &n.id),
            hop: n.hop,
            // CNV-02: the type AND the recorded range, through the SAME `effective_range` every
            // profile row is built with — one candidate, the edge's own meta, because an edge in a
            // neighbourhood has no node beneath it to fall back to. `Temporal::unfiltered` because
            // this route runs no as-at filter: an edge nobody tested against a date must not come
            // back claiming to have been in force on one.
            edges: n
                .edges
                .into_iter()
                .map(|e| NeighbourEdgeView {
                    edge_type: e.edge_type,
                    temporal: Temporal::unfiltered(effective_range(&[(
                        &e.basis,
                        e.effective_from,
                        e.effective_to,
                    )])),
                })
                .collect(),
            reached_from: n.reached_from,
        })
        .collect();

    let seeds = seed_ids
        .iter()
        .map(|id| entity_ref_for(&state.label_index, id))
        .collect();

    // The ONLY constructor: the cut and its report are one operation (see
    // `workbench_dto::neighbourhood`'s module doc). This handler could not return a shortened
    // neighbourhood claiming to be whole even if it tried — `ReportedTruncation` has no public
    // constructor, so there is no literal to write.
    Ok(Json(NeighbourhoodView::cut(seeds, hops, node_cap, nodes)))
}

/// Compile-time proof of the claim above, kept next to the handler it is about.
///
/// If `ReportedTruncation` ever gains a public constructor — a `pub` field, a `pub fn new`, a
/// `From<Truncation>` — this function starts compiling and the guard is gone. It is written as a
/// doc test rather than a `#[test]` because the thing being asserted is a *compile* failure, which
/// a runtime test cannot observe.
///
/// ```compile_fail
/// use urban_integrity_workbench_dto::algorithm::Truncation;
/// use urban_integrity_workbench_dto::neighbourhood::ReportedTruncation;
///
/// // A handler trying to say "nothing was cut" without going through `NeighbourhoodView::cut`.
/// let _lie = ReportedTruncation(Truncation {
///     truncated: false,
///     reason: None,
///     detail: String::new(),
/// });
/// ```
#[allow(dead_code)]
fn a_truncation_cannot_be_authored_outside_the_dto_crate() {}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::neighbourhood::ReportedTruncation;
    use urban_integrity_workbench_dto::EntityRef;

    fn seed() -> EntityRef {
        EntityRef {
            id: "GB-COH:X06999001".to_string(),
            name: "Example Holdings Ltd".to_string(),
            kind: "Entity".to_string(),
        }
    }

    /// The clamp is the server's. Checked here rather than only in `caps` because the clamp is
    /// what turns a constant into an enforced ceiling.
    #[test]
    fn a_request_above_the_ceiling_is_clamped_to_it() {
        let hops = 40u32.clamp(1, CEILING_NEIGHBOURHOOD_HOPS);
        let cap = 1_000_000u32.clamp(1, CEILING_NEIGHBOURHOOD_NODE_CAP);
        assert_eq!(hops, CEILING_NEIGHBOURHOOD_HOPS);
        assert_eq!(cap, CEILING_NEIGHBOURHOOD_NODE_CAP);

        // And the clamped values are what the response states, not the requested ones.
        let view = NeighbourhoodView::cut(vec![seed()], hops, cap, vec![]);
        assert_eq!(view.hops, CEILING_NEIGHBOURHOOD_HOPS);
        assert_eq!(view.node_cap, CEILING_NEIGHBOURHOOD_NODE_CAP);
    }

    /// WR-04: an `ids=` list longer than the ceiling is cut to exactly the ceiling, keeping the
    /// first-seen ids (with `id` first), after deduplication — so duplicates never eat into it.
    ///
    /// PROVE THIS CAN FAIL: drop the `list.len() >= cap` break and all 80 distinct ids come back.
    #[test]
    fn seed_ids_are_capped_server_side_at_the_ceiling_in_first_seen_order() {
        let csv: String = (0..80)
            .flat_map(|i| [format!("GB-COH:X{i:07}"), format!(" GB-COH:X{i:07} ")])
            .collect::<Vec<_>>()
            .join(",");
        let seeds = collect_seed_ids(
            Some("GB-COH:X9999999"),
            None,
            Some(&csv),
            CEILING_NEIGHBOURHOOD_SEEDS,
        );
        assert_eq!(seeds.len(), CEILING_NEIGHBOURHOOD_SEEDS);
        assert_eq!(seeds[0], "GB-COH:X9999999", "`id` is always the first seed");
        assert_eq!(seeds[1], "GB-COH:X0000000");
        assert_eq!(
            seeds[CEILING_NEIGHBOURHOOD_SEEDS - 1],
            format!("GB-COH:X{:07}", CEILING_NEIGHBOURHOOD_SEEDS - 2),
            "duplicates (here, the padded repeat of every id) never count against the cap"
        );
    }

    #[test]
    fn a_seed_list_under_the_ceiling_is_returned_whole() {
        let seeds = collect_seed_ids(None, None, Some("a, b,,a , c"), CEILING_NEIGHBOURHOOD_SEEDS);
        assert_eq!(seeds, vec!["a", "b", "c"]);
    }

    /// `ReportedTruncation` is nameable from this crate — the import in this module proves the
    /// type is public — while remaining unconstructible. Both halves matter: a type
    /// nobody can name would also be a type nobody can render.
    #[test]
    fn the_truncation_type_is_readable_here_even_though_it_is_unconstructible() {
        let view = NeighbourhoodView::cut(vec![seed()], 1, 1, vec![]);
        let block: &ReportedTruncation = &view.truncation;
        assert!(!block.truncated);
        assert_eq!(block.detail, "");
    }
}
