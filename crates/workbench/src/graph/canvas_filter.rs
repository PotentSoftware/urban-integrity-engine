//! CNV-03's five canvas filters, evaluated server-side over the baked `CsrSnapshot`.
//!
//! # Why the filter runs here and not in the browser
//!
//! Three of the five facts a filter tests are simply not in the browser. The population Arrow
//! export carries `id` / `kind` / `name` per node and `source` / `target` / `edge_type` per link
//! (`arrow_export.rs`), and `graph_page.rs` parses exactly one column of it — the ids. A node's
//! ward, a node's `P_c` and an edge's effective range are all absent, and the two ways of getting
//! them client-side are both worse than this one: widening the Arrow export puts five more
//! columns of 358,484 nodes and 507,179 links through a hand-written FlatBuffers reader that
//! already has one documented silent-empty-result defect in its history, and a per-node fetch is
//! the N+1 shape every read path in this phase has avoided.
//!
//! The snapshot is already in memory, already indexed, and already the thing every phase-44
//! algorithm traverses. The `P_c` map is the one thing that is not, and it costs one scan of the
//! `MotifDetected` rows — 1,947 of them on the live corpus, bounded by the alert corpus rather
//! than by the graph.
//!
//! # The order the filters run in, and why the order is part of the contract
//!
//! A node excluded by three filters has to be counted against exactly one of them, or the
//! empty-canvas sentence turns into a list of suspects. So the order is fixed and stated:
//!
//! 1. **node type** — a total property of every node; nothing is untestable.
//! 2. **ward** — `Represents` membership.
//! 3. **`P_c`** — the threshold.
//! 4. **date range** — over the edges induced on the nodes that survived 1-3.
//! 5. **edge type** — over the edges that survived 4.
//!
//! Steps 4 and 5 remove *edges*; a node falls out of them only when it **had** at least one
//! induced edge and has none left. A node with no induced edges at all is never removed by either,
//! for the same reason an undated edge is not removed by a date range: it was not tested, and a
//! filter that removes what it could not test is asserting something the record does not say.
//!
//! The consequence is an invariant worth more than the prose: the five counts **partition** the
//! removal, so `removed.total() == candidate_nodes - passed_before_cap` exactly. It is asserted,
//! non-vacuously, in this module's own tests.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};

use urban_integrity_workbench_dto::algorithm::Truncation;
use urban_integrity_workbench_dto::canvas_filter::{
    CanvasFilterRequest, CanvasFilterView, FacetCountView, FilterFacets, FilterRemovalCounts,
    FilteredEdgeView, MissingEvidence, WardOptionView,
};
use urban_integrity_workbench_dto::caps::CANVAS_FILTER_NODE_CAP;
use urban_integrity_workbench_dto::EntityRef;

/// The `NodeWeight` kind string a ward carries in the Arrow `kind` column and in every `EntityRef`
/// this surface emits (`arrow_export::node_kind_name`). Named once here because the ward filter's
/// whole definition rests on it.
pub const WARD_KIND: &str = "Ward";

/// The edge type that joins a ward to what it represents.
///
/// This is not a guess about the domain: it is the only `Represents` edge this project writes.
/// `ingestor::backfill::ward` emits `ward:{code} -> contract` for the buyer ward and again for a
/// differing supplier ward, and `ingestor::planning::mapper` emits `ward:{code} -> contract` for a
/// planning application. So ward membership means "joined to this ward by a `Represents` edge",
/// and the filter says exactly that rather than implying a geography it does not hold.
pub const WARD_EDGE: &str = "Represents";

/// Everything the five filters need to know about one candidate node.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeFacts {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Every ward this node is joined to by a [`WARD_EDGE`], in either direction. A ward node is
    /// a member of itself, so picking a ward never hides the ward.
    pub ward_ids: Vec<String>,
    /// The highest live `MotifDetected` score naming this node, or `None` for a node no alert
    /// names. `None` is **not** zero: "no alert looked at this" and "an alert scored this 0.0"
    /// are different statements, and the threshold treats them differently.
    pub p_c: Option<f64>,
}

/// Everything the two edge filters need to know about one candidate edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeFacts {
    pub from: String,
    pub to: String,
    pub relation: String,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
}

impl EdgeFacts {
    fn states_a_date(&self) -> bool {
        self.effective_from.is_some() || self.effective_to.is_some()
    }

    fn view(&self) -> FilteredEdgeView {
        FilteredEdgeView {
            from: self.from.clone(),
            to: self.to.clone(),
            relation: self.relation.clone(),
        }
    }
}

/// `YYYY-MM-DD` at the start of that day, or a full RFC 3339 instant.
///
/// The two-form parse and the end-of-day upper bound are `queries::select_alerts`'s own
/// convention, reused rather than re-invented: this project already has one meaning for "the
/// analyst typed a date as the end of a range", and a second one would make the same date mean
/// two things on two screens.
fn parse_from_bound(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|ndt| ndt.and_utc())
        })
}

fn parse_to_bound(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(23, 59, 59))
                .map(|ndt| ndt.and_utc())
        })
}

/// Does this edge's recorded range overlap the window?
///
/// An edge with no `effective_from` is open at the start and an edge with no `effective_to` is open
/// at the end — which on this corpus is **every** edge, since `effective_to` is null on all
/// 511,754 `EdgeAdded` events. Treating an absent bound as infinity rather than as "now" is 45-12's
/// rule: the store cannot support `-> present`, so nothing here may assert it.
fn range_overlaps(
    edge: &EdgeFacts,
    window_start: Option<DateTime<Utc>>,
    window_end: Option<DateTime<Utc>>,
) -> bool {
    let starts_before_window_ends = match (edge.effective_from, window_end) {
        (Some(start), Some(end)) => start <= end,
        _ => true,
    };
    let ends_after_window_starts = match (edge.effective_to, window_start) {
        (Some(end), Some(start)) => end >= start,
        _ => true,
    };
    starts_before_window_ends && ends_after_window_starts
}

/// The facets the pickers draw their options from — measured over the candidate set, before any
/// filter runs.
pub fn facets(nodes: &[NodeFacts], edges: &[EdgeFacts]) -> FilterFacets {
    let mut kind_counts: BTreeMap<&str, u64> = BTreeMap::new();
    for node in nodes {
        *kind_counts.entry(node.kind.as_str()).or_default() += 1;
    }
    let mut edge_counts: BTreeMap<&str, u64> = BTreeMap::new();
    for edge in edges {
        *edge_counts.entry(edge.relation.as_str()).or_default() += 1;
    }

    // A ward option's member count is over the candidate set, so an analyst picking one can see
    // what the pick would leave before making it.
    let mut ward_members: BTreeMap<&str, u64> = BTreeMap::new();
    let mut ward_names: HashMap<&str, (&str, &str)> = HashMap::new();
    for node in nodes {
        if node.kind == WARD_KIND {
            ward_names.insert(node.id.as_str(), (node.name.as_str(), node.kind.as_str()));
        }
        for ward in &node.ward_ids {
            *ward_members.entry(ward.as_str()).or_default() += 1;
        }
    }

    let wards = ward_members
        .into_iter()
        .map(|(id, member_count)| {
            let (name, kind) = ward_names
                .get(id)
                // A ward that no candidate node *is* (only one that candidate nodes belong to)
                // still has to be offerable, and the only name available then is its own id.
                // Never a fabricated label.
                .copied()
                .unwrap_or((id, WARD_KIND));
            WardOptionView {
                entity: EntityRef {
                    id: id.to_string(),
                    name: name.to_string(),
                    kind: kind.to_string(),
                },
                member_count,
            }
        })
        .collect();

    let scores: Vec<f64> = nodes.iter().filter_map(|n| n.p_c).collect();
    let edges_with_a_date = edges.iter().filter(|e| e.states_a_date()).count() as u64;

    FilterFacets {
        node_kinds: sorted_facets(kind_counts),
        edge_types: sorted_facets(edge_counts),
        wards,
        nodes_with_a_score: scores.len() as u64,
        highest_score: scores.iter().copied().fold(None, |acc: Option<f64>, s| {
            Some(acc.map_or(s, |cur| cur.max(s)))
        }),
        edges_with_a_date,
        edges_without_a_date: edges.len() as u64 - edges_with_a_date,
    }
}

/// Most common first, then alphabetically — a total order, so the checklist does not reshuffle
/// between two requests over the same data.
fn sorted_facets(counts: BTreeMap<&str, u64>) -> Vec<FacetCountView> {
    let mut out: Vec<FacetCountView> = counts
        .into_iter()
        .map(|(value, count)| FacetCountView {
            value: value.to_string(),
            count,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    out
}

/// Run the five filters over one candidate set.
///
/// Pure: no snapshot, no database, no clock. Every fact it tests arrives in `nodes` / `edges`,
/// which is what makes the partition invariant and the empty-canvas cases testable without a
/// running server.
pub fn evaluate(
    nodes: &[NodeFacts],
    edges: &[EdgeFacts],
    request: &CanvasFilterRequest,
) -> CanvasFilterView {
    let facets = facets(nodes, edges);
    let available_kinds: Vec<String> = facets.node_kinds.iter().map(|f| f.value.clone()).collect();
    let available_edge_types: Vec<String> =
        facets.edge_types.iter().map(|f| f.value.clone()).collect();
    let active = request.is_active(&available_kinds, &available_edge_types);

    if !active {
        return CanvasFilterView {
            filter_active: false,
            candidate_nodes: nodes.len() as u64,
            candidate_edges: edges.len() as u64,
            node_ids: Vec::new(),
            edges: Vec::new(),
            removed: FilterRemovalCounts::default(),
            facets,
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            passed_before_cap: nodes.len() as u64,
        };
    }

    let mut removed = FilterRemovalCounts::default();
    let kind_set: HashSet<&str> = request.node_kinds.iter().map(String::as_str).collect();
    let edge_type_set: HashSet<&str> = request.edge_types.iter().map(String::as_str).collect();

    // ── 1-3: the node filters, in order, each counted against the first that would remove ──
    let mut kept: Vec<&NodeFacts> = Vec::with_capacity(nodes.len());
    for node in nodes {
        if !kind_set.contains(node.kind.as_str()) {
            removed.by_node_type += 1;
            continue;
        }
        if let Some(ward) = &request.ward_id {
            if !node.ward_ids.iter().any(|w| w == ward) {
                removed.by_ward += 1;
                continue;
            }
        }
        if let Some(threshold) = request.min_p_c {
            match node.p_c {
                Some(score) if score >= threshold => {}
                Some(_) => {
                    removed.by_p_c += 1;
                    continue;
                }
                // No alert names this node, so there is no score to compare. Whether that is a
                // reason to remove it is the analyst's position, taken on a control, never a
                // default nobody saw.
                None => {
                    if request.unscored_nodes == MissingEvidence::Exclude {
                        removed.by_p_c += 1;
                        continue;
                    }
                }
            }
        }
        kept.push(node);
    }

    let kept_ids: HashSet<&str> = kept.iter().map(|n| n.id.as_str()).collect();

    // ── 4: the date range, over the edges induced on the survivors ──
    let window_start = request.from_date.as_deref().and_then(parse_from_bound);
    let window_end = request.to_date.as_deref().and_then(parse_to_bound);
    let date_active = request.from_date.is_some() || request.to_date.is_some();

    let induced: Vec<&EdgeFacts> = edges
        .iter()
        .filter(|e| kept_ids.contains(e.from.as_str()) && kept_ids.contains(e.to.as_str()))
        .collect();

    let dated_ok: Vec<&EdgeFacts> = induced
        .iter()
        .copied()
        .filter(|e| {
            if !date_active {
                return true;
            }
            if !e.states_a_date() {
                return request.undated_edges == MissingEvidence::Keep;
            }
            range_overlaps(e, window_start, window_end)
        })
        .collect();

    // ── 5: the edge-type checklist, over what the date range left ──
    let typed_ok: Vec<&EdgeFacts> = dated_ok
        .iter()
        .copied()
        .filter(|e| edge_type_set.contains(e.relation.as_str()))
        .collect();

    let had_an_edge: HashSet<&str> = endpoints(&induced);
    let survived_dates: HashSet<&str> = endpoints(&dated_ok);
    let survived_types: HashSet<&str> = endpoints(&typed_ok);

    let mut node_ids: Vec<String> = Vec::with_capacity(kept.len());
    for node in &kept {
        let id = node.id.as_str();
        if !had_an_edge.contains(id) {
            // Untestable by either edge filter — it had no induced edge to test. Kept.
            node_ids.push(node.id.clone());
            continue;
        }
        if !survived_dates.contains(id) {
            removed.by_date_range += 1;
            continue;
        }
        if !survived_types.contains(id) {
            removed.by_edge_type += 1;
            continue;
        }
        node_ids.push(node.id.clone());
    }

    let passed_before_cap = node_ids.len() as u64;
    let cap = CANVAS_FILTER_NODE_CAP as usize;
    let cut = node_ids.len().saturating_sub(cap);
    node_ids.truncate(cap);
    let visible: HashSet<&str> = node_ids.iter().map(String::as_str).collect();

    let mut visible_edges: Vec<FilteredEdgeView> = typed_ok
        .iter()
        .filter(|e| visible.contains(e.from.as_str()) && visible.contains(e.to.as_str()))
        .map(|e| e.view())
        .collect();
    visible_edges.sort();
    visible_edges.dedup();

    let truncation = if cut == 0 {
        Truncation {
            truncated: false,
            reason: None,
            detail: String::new(),
        }
    } else {
        Truncation {
            truncated: true,
            reason: Some("canvas_filter_node_cap".to_string()),
            detail: format!(
                "stopped at canvas filter node cap {CANVAS_FILTER_NODE_CAP}: {cut} of \
                 {passed_before_cap} nodes that passed the filter are not shown"
            ),
        }
    };

    CanvasFilterView {
        filter_active: true,
        candidate_nodes: nodes.len() as u64,
        candidate_edges: edges.len() as u64,
        node_ids,
        edges: visible_edges,
        removed,
        facets,
        truncation,
        passed_before_cap,
    }
}

/// **The visible subgraph**: what is on the canvas, nodes and edges, under this request.
///
/// The same answer [`evaluate`] gives when the request narrows something, and the whole candidate
/// set when it does not — because "no filter" still leaves a canvas, and CNV-03's save has to be
/// able to write what an unfiltered expansion was showing.
///
/// This is the server's own answer to "what was visible", computed independently of the browser's.
/// `crate::investigation::save_subgraph_handler` writes pins only when the two agree.
pub fn visible_subgraph(
    nodes: &[NodeFacts],
    edges: &[EdgeFacts],
    request: &CanvasFilterRequest,
) -> (Vec<String>, Vec<FilteredEdgeView>) {
    let view = evaluate(nodes, edges, request);
    if view.filter_active {
        return (view.node_ids, view.edges);
    }
    let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut induced: Vec<FilteredEdgeView> = edges.iter().map(|e| e.view()).collect();
    induced.sort();
    induced.dedup();
    (ids, induced)
}

fn endpoints<'a>(edges: &[&'a EdgeFacts]) -> HashSet<&'a str> {
    let mut out: HashSet<&str> = HashSet::with_capacity(edges.len() * 2);
    for edge in edges {
        out.insert(edge.from.as_str());
        out.insert(edge.to.as_str());
    }
    out
}

// ─── Gathering the facts out of the baked snapshot ─────────────────────────────────────────────

/// The candidate set one filter request runs over.
pub struct Candidates {
    pub nodes: Vec<NodeFacts>,
    pub edges: Vec<EdgeFacts>,
}

/// The highest live alert score naming each entity — the `P_c` the threshold filter tests.
///
/// **Maximum, not mean or latest**, because that is what `build_verify_verdict` already means by
/// an entity's `p_c_score` ("the worst-case signal wins"), and two screens in one product must not
/// mean two things by one name.
///
/// A withdrawn alert is not counted: `query_alerts` drops it, and an operator who retracted a
/// finding has said it should not raise a score.
pub fn scores_by_entity(
    alerts: &[urban_integrity_core::persistence::queries::AlertRecord],
) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for alert in alerts {
        let Ok(result) = serde_json::from_str::<serde_json::Value>(&alert.result_json) else {
            continue;
        };
        let Some(participants) = result["participants"].as_array() else {
            continue;
        };
        for participant in participants {
            let Some(id) = participant.as_str() else {
                continue;
            };
            out.entry(id.to_string())
                .and_modify(|cur| {
                    if alert.score > *cur {
                        *cur = alert.score;
                    }
                })
                .or_insert(alert.score);
        }
    }
    out
}

/// Build the candidate node and edge facts from the snapshot.
///
/// `scope_ids` empty means the whole population scope; otherwise the candidates are those of
/// `scope_ids` that are actually in the population (an id the canvas is focused on that is outside
/// the population cannot be filtered *into* view, and silently admitting it would let a filter
/// widen the canvas).
///
/// # Cost
///
/// One pass over the population's nodes and one over their outgoing edges, materialising both as
/// owned facts — the same shape and the same order of magnitude as `arrow_export::build_arrow_ipc`,
/// which builds `Vec<String>` of every node id and every link endpoint on each population request.
/// It runs inside `spawn_blocking` for the same reason that one does.
pub fn gather(
    storage: &urban_integrity_core::graph::storage::GraphStorage,
    snapshot: &urban_integrity_core::graph::snapshot::CsrSnapshot,
    scope: &crate::graph::population_scope::PopulationScope,
    scope_ids: &[String],
    scores: &HashMap<String, f64>,
) -> Candidates {
    use petgraph::visit::EdgeRef;
    use urban_integrity_schema::types::id::EntityId;

    let mut id_by_idx: HashMap<u32, String> = HashMap::new();
    let mut kind_by_idx: HashMap<u32, &'static str> = HashMap::new();
    let mut name_by_idx: HashMap<u32, String> = HashMap::new();

    if scope_ids.is_empty() {
        for (id, node_idx, weight, _) in storage.snapshot_nodes() {
            let idx = node_idx.index() as u32;
            if !scope.node_ids.contains(&idx) {
                continue;
            }
            kind_by_idx.insert(idx, crate::graph::arrow_export::node_kind_name(&weight));
            name_by_idx.insert(idx, crate::graph::arrow_export::node_display_name(&weight));
            id_by_idx.insert(idx, id.0);
        }
    } else {
        for id in scope_ids {
            let Some(entry) = storage.node_map.get(&EntityId(id.clone())) else {
                continue;
            };
            let (node_idx, weight, _) = entry.value().clone();
            drop(entry);
            let idx = node_idx.index() as u32;
            if !scope.node_ids.contains(&idx) {
                continue;
            }
            kind_by_idx.insert(idx, crate::graph::arrow_export::node_kind_name(&weight));
            name_by_idx.insert(idx, crate::graph::arrow_export::node_display_name(&weight));
            id_by_idx.insert(idx, id.clone());
        }
    }

    // Induced edges: each considered exactly once, from its source's own outgoing list — the same
    // pattern `arrow_export` and `csr_view::build_scoped_adjacency` use, and the thing that keeps
    // every emitted endpoint a candidate id.
    let mut edges: Vec<EdgeFacts> = Vec::new();
    for (&idx, from_id) in &id_by_idx {
        for edge_ref in snapshot.graph.edges(idx) {
            let target = edge_ref.target();
            let Some(to_id) = id_by_idx.get(&target) else {
                continue;
            };
            let meta = crate::graph::handlers::edge_meta(edge_ref.weight());
            edges.push(EdgeFacts {
                from: from_id.clone(),
                to: to_id.clone(),
                relation: crate::graph::csr_view::edge_type_name(edge_ref.weight()).to_string(),
                effective_from: meta.effective_from,
                effective_to: meta.effective_to,
            });
        }
    }

    // Ward membership, from the `Represents` edges among the candidates. A ward that is not itself
    // in view cannot be picked (the facets are measured over the candidates too), so membership is
    // measured over the same set the picker offers — one definition, not two.
    let mut wards_by_id: HashMap<&str, Vec<String>> = HashMap::new();
    let ward_idx_ids: HashSet<&str> = id_by_idx
        .iter()
        .filter(|(idx, _)| kind_by_idx.get(idx).copied() == Some(WARD_KIND))
        .map(|(_, id)| id.as_str())
        .collect();
    for edge in &edges {
        if edge.relation != WARD_EDGE {
            continue;
        }
        if ward_idx_ids.contains(edge.from.as_str()) {
            wards_by_id
                .entry(edge.to.as_str())
                .or_default()
                .push(edge.from.clone());
        }
    }

    let mut nodes: Vec<NodeFacts> = id_by_idx
        .iter()
        .map(|(idx, id)| {
            let mut ward_ids = wards_by_id.get(id.as_str()).cloned().unwrap_or_default();
            // A ward is a member of itself, so picking one never hides the thing picked.
            if ward_idx_ids.contains(id.as_str()) {
                ward_ids.push(id.clone());
            }
            ward_ids.sort();
            ward_ids.dedup();
            NodeFacts {
                id: id.clone(),
                name: name_by_idx.get(idx).cloned().unwrap_or_else(|| id.clone()),
                kind: kind_by_idx
                    .get(idx)
                    .copied()
                    .unwrap_or("Entity")
                    .to_string(),
                ward_ids,
                p_c: scores.get(id.as_str()).copied(),
            }
        })
        .collect();

    // A total order, so two requests over the same data return the same window when the cap bites.
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.relation.cmp(&b.relation))
    });

    Candidates { nodes, edges }
}

// ─── The route ────────────────────────────────────────────────────────────────────────────────

/// `POST /api/graph/canvas-filter` — CNV-03's filter, and the facets its pickers are built from.
///
/// `POST` rather than `GET` because the request is a structured object with two checklists in it,
/// and the canvas's focus set travels with it: the same reason `/api/graph/algorithm` is a POST.
/// It selects **within** the already-fixed population and never names a database.
///
/// A request whose filters would narrow nothing is answered with the facets and an empty id list
/// (`filter_active: false`) — the canvas already knows its own focus set, and three hundred
/// thousand ids echoed back to confirm it is a payload with no reader.
pub async fn canvas_filter_handler(
    axum::Extension(handle): axum::Extension<
        std::sync::Arc<urban_integrity_core::graph::coordinator::GraphCoordinatorHandle>,
    >,
    axum::Extension(state): axum::Extension<std::sync::Arc<crate::boot::WorkbenchState>>,
    axum::response::Json(request): axum::response::Json<CanvasFilterRequest>,
) -> Result<axum::response::Json<CanvasFilterView>, crate::entity::ApiError> {
    use urban_integrity_core::persistence::queries::{query_alerts, AlertFilter};

    let Some(scope) = crate::boot::resolve_population_scope(&state, &handle) else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::response::Json(serde_json::json!({
                "error": "population not yet available: the graph snapshot is still baking — retry shortly"
            })),
        ));
    };
    let scope = scope.clone();

    // One scan of the alert corpus (1,947 `MotifDetected` rows on the live analysis), never one
    // read per node. The `Err` is returned, never swallowed: a filter that silently scored every
    // node `None` would quietly remove every node the threshold was meant to keep.
    let alerts = query_alerts(&state.db, &AlertFilter::default())
        .await
        .map_err(|e| crate::entity::internal(format!("alert scores unavailable: {e}")))?;
    let scores = scores_by_entity(&alerts.alerts);

    let storage = handle.storage.clone();
    let snapshot = std::sync::Arc::clone(&handle.snapshot.load());
    let view = tokio::task::spawn_blocking(move || {
        let candidates = gather(&storage, &snapshot, &scope, &request.scope_ids, &scores);
        evaluate(&candidates.nodes, &candidates.edges, &request)
    })
    .await
    .map_err(|e| crate::entity::internal(format!("canvas filter failed: {e}")))?;

    Ok(axum::response::Json(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: &str, wards: &[&str], p_c: Option<f64>) -> NodeFacts {
        NodeFacts {
            id: id.to_string(),
            name: format!("{id} name"),
            kind: kind.to_string(),
            ward_ids: wards.iter().map(|w| w.to_string()).collect(),
            p_c,
        }
    }

    fn edge(from: &str, to: &str, relation: &str) -> EdgeFacts {
        EdgeFacts {
            from: from.to_string(),
            to: to.to_string(),
            relation: relation.to_string(),
            effective_from: None,
            effective_to: None,
        }
    }

    fn dated(from: &str, to: &str, relation: &str, start: &str, end: Option<&str>) -> EdgeFacts {
        EdgeFacts {
            from: from.to_string(),
            to: to.to_string(),
            relation: relation.to_string(),
            effective_from: Some(
                DateTime::parse_from_rfc3339(start)
                    .expect("fixture start must parse")
                    .with_timezone(&Utc),
            ),
            effective_to: end.map(|e| {
                DateTime::parse_from_rfc3339(e)
                    .expect("fixture end must parse")
                    .with_timezone(&Utc)
            }),
        }
    }

    /// Everything ticked, nothing else set — the inactive baseline every test narrows from.
    fn all_ticked(nodes: &[NodeFacts], edges: &[EdgeFacts]) -> CanvasFilterRequest {
        let f = facets(nodes, edges);
        CanvasFilterRequest {
            node_kinds: f.node_kinds.iter().map(|k| k.value.clone()).collect(),
            edge_types: f.edge_types.iter().map(|e| e.value.clone()).collect(),
            from_date: None,
            to_date: None,
            undated_edges: MissingEvidence::Keep,
            ward_id: None,
            min_p_c: None,
            unscored_nodes: MissingEvidence::Exclude,
            scope_ids: vec![],
        }
    }

    /// A small graph with two wards, a scored entity, an unscored one, and one dated edge among
    /// several undated ones — every fact the five filters test, present in one fixture so a
    /// filter's effect can be measured against the others staying put.
    fn fixture() -> (Vec<NodeFacts>, Vec<EdgeFacts>) {
        let nodes = vec![
            node("ward:X05009901", "Ward", &["ward:X05009901"], None),
            node("ward:X05009902", "Ward", &["ward:X05009902"], None),
            node("contract:c1", "Contract", &["ward:X05009901"], None),
            node("contract:c2", "Contract", &["ward:X05009902"], None),
            node("GB-COH:X6999001", "Entity", &[], Some(0.91)),
            node("GB-COH:X6999002", "Entity", &[], Some(0.42)),
            node("OFF:jane-example", "Officer", &[], None),
            node("LOC:X001AA:example-street", "Location", &[], None),
        ];
        let edges = vec![
            edge("ward:X05009901", "contract:c1", "Represents"),
            edge("ward:X05009902", "contract:c2", "Represents"),
            edge("contract:c1", "GB-COH:X6999001", "AwardedTo"),
            edge("contract:c2", "GB-COH:X6999002", "AwardedTo"),
            edge("OFF:jane-example", "GB-COH:X6999001", "Directs"),
            edge("GB-COH:X6999001", "LOC:X001AA:example-street", "LocatedAt"),
            dated(
                "GB-COH:X6999002",
                "GB-COH:X6999001",
                "Controls",
                "2019-04-01T00:00:00Z",
                None,
            ),
        ];
        (nodes, edges)
    }

    fn ids(view: &CanvasFilterView) -> Vec<&str> {
        view.node_ids.iter().map(String::as_str).collect()
    }

    /// The pickers' options are **measured over the candidate set**, counts and all — never a list
    /// kept by hand in the browser.
    ///
    /// PROVE THIS CAN FAIL: making `sorted_facets` return an empty `Vec` fails the first two
    /// assertions; dropping the self-membership line in `facets`'s ward fold fails the ward count.
    #[test]
    fn every_picker_option_is_measured_from_the_candidate_set() {
        let (nodes, edges) = fixture();
        let f = facets(&nodes, &edges);

        assert_eq!(
            f.node_kinds
                .iter()
                .map(|k| (k.value.as_str(), k.count))
                .collect::<Vec<_>>(),
            vec![
                ("Contract", 2),
                ("Entity", 2),
                ("Ward", 2),
                ("Location", 1),
                ("Officer", 1),
            ],
            "most common first, then alphabetically — a total order"
        );
        assert_eq!(
            f.edge_types
                .iter()
                .map(|e| (e.value.as_str(), e.count))
                .collect::<Vec<_>>(),
            vec![
                ("AwardedTo", 2),
                ("Represents", 2),
                ("Controls", 1),
                ("Directs", 1),
                ("LocatedAt", 1),
            ]
        );

        // Each ward represents one contract and is a member of itself.
        assert_eq!(f.wards.len(), 2);
        assert!(f.wards.iter().all(|w| w.member_count == 2));
        assert_eq!(f.wards[0].entity.id, "ward:X05009901");
        assert_eq!(f.wards[0].entity.name, "ward:X05009901 name");

        assert_eq!(f.nodes_with_a_score, 2);
        assert_eq!(f.highest_score, Some(0.91));
        assert_eq!(f.edges_with_a_date, 1);
        assert_eq!(f.edges_without_a_date, 6);
    }

    /// The five counts are a **partition** of the removal: whatever combination runs, they sum to
    /// exactly what came out.
    ///
    /// Swept over nine filter combinations with non-vacuity asserted on both arms — at least three
    /// must remove something and at least one must remove nothing — so a sweep that only ever
    /// filtered, or only ever passed, fails before it can prove half of what it claims.
    ///
    /// PROVE THIS CAN FAIL: making the `by_ward` arm `continue` without incrementing its counter
    /// breaks the sum on every ward case.
    #[test]
    fn the_five_removal_counts_partition_the_removal() {
        let (nodes, edges) = fixture();
        let base = all_ticked(&nodes, &edges);

        let mut cases: Vec<CanvasFilterRequest> = Vec::new();
        cases.push(base.clone());
        for kinds in [
            vec!["Entity"],
            vec!["Entity", "Contract"],
            vec![],
            vec!["Entity", "Contract", "Ward", "Officer", "Location"],
        ] {
            let mut c = base.clone();
            c.node_kinds = kinds.iter().map(|k| k.to_string()).collect();
            cases.push(c);
        }
        let mut ward = base.clone();
        ward.ward_id = Some("ward:X05009901".to_string());
        cases.push(ward);
        let mut scored = base.clone();
        scored.min_p_c = Some(0.5);
        cases.push(scored);
        let mut scored_keep = base.clone();
        scored_keep.min_p_c = Some(0.5);
        scored_keep.unscored_nodes = MissingEvidence::Keep;
        cases.push(scored_keep);
        let mut windowed = base.clone();
        windowed.from_date = Some("2020-01-01".to_string());
        windowed.undated_edges = MissingEvidence::Exclude;
        cases.push(windowed);
        let mut typed = base.clone();
        typed.edge_types = vec!["AwardedTo".to_string()];
        cases.push(typed);

        let mut removing = 0;
        let mut passing = 0;
        for case in &cases {
            let view = evaluate(&nodes, &edges, case);
            if !view.filter_active {
                passing += 1;
                assert_eq!(view.removed.total(), 0);
                continue;
            }
            assert_eq!(
                view.removed.total(),
                view.candidate_nodes - view.passed_before_cap,
                "the five counts must partition the removal for {case:?}"
            );
            if view.removed.total() > 0 {
                removing += 1;
            } else {
                passing += 1;
            }
        }
        assert!(
            removing >= 3,
            "non-vacuity: at least three cases must actually remove something, got {removing}"
        );
        assert!(
            passing >= 1,
            "non-vacuity: at least one case must remove nothing, got {passing}"
        );
    }

    /// Unticking a node kind removes exactly the nodes of that kind, and nothing else moves.
    ///
    /// PROVE THIS CAN FAIL: inverting the `kind_set.contains` test empties the result.
    #[test]
    fn the_node_type_checklist_removes_exactly_the_kinds_it_untickets() {
        let (nodes, edges) = fixture();
        let mut request = all_ticked(&nodes, &edges);
        request.node_kinds = vec!["Entity".to_string(), "Contract".to_string()];

        let view = evaluate(&nodes, &edges, &request);
        assert!(view.filter_active);
        assert_eq!(
            view.removed.by_node_type, 4,
            "two wards, an officer, a location"
        );
        assert_eq!(
            ids(&view),
            // Candidate order is preserved — the window a cap keeps is the nearest one, not an
            // arbitrary one, so `evaluate` never reshuffles what it was handed.
            vec![
                "contract:c1",
                "contract:c2",
                "GB-COH:X6999001",
                "GB-COH:X6999002",
            ]
        );
        // The edge set follows the nodes: an edge with an excluded endpoint is not visible.
        assert!(view
            .edges
            .iter()
            .all(|e| e.relation == "AwardedTo" || e.relation == "Controls"));
    }

    /// An undated edge is **kept** by a date range unless the analyst says otherwise, and the
    /// analyst's other position genuinely removes it.
    ///
    /// This is the rule the corpus forces: `effective_to` is null on every `EdgeAdded` event in the
    /// store, so a date range that dropped undated edges by default would empty the canvas on
    /// almost any input and read as a finding.
    ///
    /// PROVE THIS CAN FAIL: replacing the `states_a_date` branch with `false` (always drop the
    /// undated) makes the Keep arm fail; replacing it with `true` makes the Exclude arm fail.
    #[test]
    fn an_undated_edge_survives_a_date_range_unless_the_analyst_excludes_it() {
        let (nodes, edges) = fixture();
        // Non-vacuity: the fixture must actually contain both kinds of edge, or neither arm below
        // proves anything.
        let f = facets(&nodes, &edges);
        assert!(f.edges_with_a_date > 0 && f.edges_without_a_date > 0);

        let mut keep = all_ticked(&nodes, &edges);
        keep.from_date = Some("2019-01-01".to_string());
        keep.to_date = Some("2019-12-31".to_string());
        keep.undated_edges = MissingEvidence::Keep;
        let kept = evaluate(&nodes, &edges, &keep);
        assert_eq!(
            kept.removed.by_date_range, 0,
            "no node may fall out for holding an edge whose record states no date"
        );
        assert_eq!(kept.node_ids.len(), nodes.len());

        let mut exclude = keep.clone();
        exclude.undated_edges = MissingEvidence::Exclude;
        let excluded = evaluate(&nodes, &edges, &exclude);
        assert!(
            excluded.removed.by_date_range > 0,
            "excluding the undated must actually remove something, or the control is decoration"
        );
        // The one dated edge is in the window, so its two endpoints survive and everything whose
        // only edges were undated does not.
        assert_eq!(ids(&excluded), vec!["GB-COH:X6999001", "GB-COH:X6999002"]);
    }

    /// A window that the one dated edge falls outside of removes it, and an open-ended edge is not
    /// asserted to have ended.
    ///
    /// PROVE THIS CAN FAIL: making `range_overlaps` return `true` unconditionally fails the first
    /// assertion; treating a `None` `effective_to` as "ended at `effective_from`" fails the second.
    #[test]
    fn a_dated_edge_is_tested_against_the_window_and_an_open_end_is_never_closed() {
        let (nodes, edges) = fixture();
        let mut before = all_ticked(&nodes, &edges);
        before.from_date = Some("2010-01-01".to_string());
        before.to_date = Some("2015-12-31".to_string());
        before.undated_edges = MissingEvidence::Exclude;
        let view = evaluate(&nodes, &edges, &before);
        assert!(
            view.node_ids.is_empty(),
            "the only dated edge starts in 2019 and every other edge was excluded"
        );

        let mut after = before.clone();
        after.from_date = Some("2024-01-01".to_string());
        after.to_date = Some("2024-12-31".to_string());
        let still_open = evaluate(&nodes, &edges, &after);
        assert_eq!(
            ids(&still_open),
            vec!["GB-COH:X6999001", "GB-COH:X6999002"],
            "an edge with no recorded end has not ended, so it is still in force in 2024"
        );
    }

    /// A `P_c` threshold keeps only what scores at or above it, and what happens to a node no
    /// alert ever named is the analyst's own choice.
    ///
    /// PROVE THIS CAN FAIL: treating `p_c: None` as `0.0` makes the Keep arm fail (every unscored
    /// node would be removed whatever the analyst picked).
    #[test]
    fn a_score_threshold_tests_only_what_carries_a_score() {
        let (nodes, edges) = fixture();
        let mut exclude = all_ticked(&nodes, &edges);
        exclude.min_p_c = Some(0.5);
        exclude.unscored_nodes = MissingEvidence::Exclude;
        let strict = evaluate(&nodes, &edges, &exclude);
        assert_eq!(ids(&strict), vec!["GB-COH:X6999001"]);
        assert_eq!(strict.removed.by_p_c, 7);

        let mut keep = exclude.clone();
        keep.unscored_nodes = MissingEvidence::Keep;
        let lenient = evaluate(&nodes, &edges, &keep);
        assert_eq!(
            lenient.removed.by_p_c, 1,
            "only the node that scored 0.42 was tested and failed"
        );
        assert!(lenient.node_ids.contains(&"OFF:jane-example".to_string()));
        assert!(!lenient.node_ids.contains(&"GB-COH:X6999002".to_string()));
    }

    /// Picking a ward keeps the ward and what it represents, and nothing else.
    ///
    /// PROVE THIS CAN FAIL: dropping the ward's self-membership in `gather` would hide the ward
    /// itself; here, inverting the `ward_ids.iter().any` test empties the result.
    #[test]
    fn a_ward_pick_keeps_the_ward_and_what_it_represents() {
        let (nodes, edges) = fixture();
        let mut request = all_ticked(&nodes, &edges);
        request.ward_id = Some("ward:X05009901".to_string());
        let view = evaluate(&nodes, &edges, &request);
        assert_eq!(ids(&view), vec!["ward:X05009901", "contract:c1"]);
        assert_eq!(view.removed.by_ward, 6);
        assert_eq!(
            view.edges,
            vec![FilteredEdgeView {
                from: "ward:X05009901".to_string(),
                to: "contract:c1".to_string(),
                relation: "Represents".to_string(),
            }]
        );
    }

    /// An edge-type checklist removes a node that **had** an edge and lost every one of them, and
    /// never a node it could not test.
    ///
    /// PROVE THIS CAN FAIL: removing the `had_an_edge` guard makes the isolated node disappear,
    /// failing the second assertion.
    #[test]
    fn an_edge_type_checklist_never_removes_a_node_it_could_not_test() {
        let (mut nodes, edges) = fixture();
        nodes.push(node("GB-COH:X6999003", "Entity", &[], None));
        let mut request = all_ticked(&nodes, &edges);
        request.edge_types = vec!["AwardedTo".to_string()];
        let view = evaluate(&nodes, &edges, &request);

        assert!(
            !view.node_ids.contains(&"OFF:jane-example".to_string()),
            "its only edge was a Directs, which is unticked"
        );
        assert!(
            view.node_ids.contains(&"GB-COH:X6999003".to_string()),
            "a node with no edge at all was never tested by an edge filter, so it stays"
        );
        assert!(view.removed.by_edge_type > 0);
    }

    /// **The visible edge set is not recoverable from the visible node set.**
    ///
    /// Two filters that leave the *same* nodes on screen leave different edges between them, which
    /// is 45-07's whole reason for giving a pin an `Edge` target: a saved subgraph that re-derived
    /// its edges from its nodes would silently restore the ones the analyst filtered out.
    ///
    /// PROVE THIS CAN FAIL: making the visible edge set the induced edges (`dated_ok` rather than
    /// `typed_ok`) makes the two edge sets equal and the assertion fail.
    #[test]
    fn two_filters_can_agree_about_the_nodes_and_disagree_about_the_edges() {
        let nodes = vec![
            node("GB-COH:X6999001", "Entity", &[], None),
            node("GB-COH:X6999002", "Entity", &[], None),
        ];
        let edges = vec![
            edge("GB-COH:X6999001", "GB-COH:X6999002", "Controls"),
            edge("GB-COH:X6999001", "GB-COH:X6999002", "Directs"),
        ];

        let mut both = all_ticked(&nodes, &edges);
        both.node_kinds = vec!["Entity".to_string()];
        // Force the request active without changing the node set: one edge type unticked.
        let mut one = both.clone();
        one.edge_types = vec!["Controls".to_string()];

        let with_both = evaluate(&nodes, &edges, &{
            let mut r = both.clone();
            r.min_p_c = Some(0.0);
            r.unscored_nodes = MissingEvidence::Keep;
            r
        });
        let with_one = evaluate(&nodes, &edges, &one);

        assert_eq!(
            with_both.node_ids, with_one.node_ids,
            "non-vacuity: the two filters must leave the same nodes, or this proves nothing"
        );
        assert_eq!(with_both.edges.len(), 2);
        assert_eq!(with_one.edges.len(), 1);
        assert_ne!(with_both.edges, with_one.edges);
    }

    /// The result cap reports when it bites and stays quiet when it does not — and the figure it
    /// states is the cut, not the shown.
    ///
    /// PROVE THIS CAN FAIL: appending the truncation unconditionally fails the quiet arm; printing
    /// `node_ids.len()` in `detail` instead of `cut` fails the biting arm.
    #[test]
    fn the_filter_result_cap_reports_when_it_bites_and_is_quiet_when_it_does_not() {
        let big: Vec<NodeFacts> = (0..(CANVAS_FILTER_NODE_CAP as usize + 42))
            .map(|i| node(&format!("GB-COH:X{i:07}"), "Entity", &[], None))
            .collect();
        let mut request = all_ticked(&big, &[]);
        request.min_p_c = Some(0.0);
        request.unscored_nodes = MissingEvidence::Keep;

        let cut = evaluate(&big, &[], &request);
        assert!(cut.truncation.truncated);
        assert_eq!(cut.node_ids.len(), CANVAS_FILTER_NODE_CAP as usize);
        assert_eq!(cut.passed_before_cap, CANVAS_FILTER_NODE_CAP as u64 + 42);
        assert!(
            cut.truncation.detail.contains("42 of"),
            "the server states what it cut, not what it showed: {}",
            cut.truncation.detail
        );

        let small = &big[..10];
        let fits = evaluate(small, &[], &request);
        assert!(!fits.truncation.truncated);
        assert!(fits.truncation.detail.is_empty());
        assert_eq!(fits.node_ids.len(), 10);
    }

    /// A request that narrows nothing is answered with the facets and no id list.
    ///
    /// PROVE THIS CAN FAIL: making `is_active` return `true` unconditionally makes `node_ids`
    /// non-empty and the assertion fail.
    #[test]
    fn an_inactive_filter_returns_facets_and_no_id_list() {
        let (nodes, edges) = fixture();
        let view = evaluate(&nodes, &edges, &all_ticked(&nodes, &edges));
        assert!(!view.filter_active);
        assert!(view.node_ids.is_empty());
        assert!(view.edges.is_empty());
        assert_eq!(view.candidate_nodes, nodes.len() as u64);
        assert_eq!(view.facets.node_kinds.len(), 5);
    }

    /// The `P_c` map takes the **maximum** score across the alerts naming an entity, matching
    /// `build_verify_verdict`'s own definition rather than inventing a second one.
    ///
    /// PROVE THIS CAN FAIL: replacing the `and_modify` max with a plain overwrite makes the
    /// assertion depend on alert order and fail for the lower-scored alert arriving last.
    #[test]
    fn a_nodes_score_is_the_highest_alert_that_names_it() {
        use urban_integrity_core::persistence::queries::AlertRecord;
        use urban_integrity_workbench_dto::provenance::Provenance;

        let make = |score: f64, participants: &[&str]| AlertRecord {
            id: format!("alert-{score}"),
            motif_type: "phoenix".to_string(),
            score,
            result_json: serde_json::json!({ "participants": participants }).to_string(),
            detected_at: "2026-01-01T00:00:00Z".to_string(),
            ward_code: None,
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: Provenance::Unattributed {
                reason: "fixture".to_string(),
            },
        };
        let alerts = vec![
            make(0.91, &["GB-COH:X6999001"]),
            make(0.42, &["GB-COH:X6999001", "GB-COH:X6999002"]),
        ];
        let scores = scores_by_entity(&alerts);
        assert_eq!(scores.get("GB-COH:X6999001"), Some(&0.91));
        assert_eq!(scores.get("GB-COH:X6999002"), Some(&0.42));
        assert_eq!(scores.get("GB-COH:X6999003"), None);
    }
}
