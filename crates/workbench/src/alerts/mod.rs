//! `GET /api/alerts`, `GET /api/alert/:id`, `POST /api/alert/:id/withdraw` — the alert / motif
//! triage inbox's whole data surface (TRI-01, TRI-02, plan 46-06).
//!
//! # Where the canonical 8-detector taxonomy lives, and why here
//!
//! `crates/core` cannot depend on `crates/detector` in production code — `urban-integrity-detector`
//! is a `[dev-dependencies]`-only entry in `crates/core/Cargo.toml` (verified against the tree
//! before this plan was written). `crates/core::persistence::queries::alert_facets` therefore
//! returns a generic, string-keyed [`urban_integrity_core::AlertFacetGroups`] — every wire-string
//! count exists, but nothing there knows there are exactly 8 detector types, or what to call them.
//! This module layers [`urban_integrity_detector::alert::result::MotifType::ALL`] on top: the ONE
//! place this phase enumerates "which detectors exist" and "what a `MotifKind::SingleSubject`
//! alert means for path rendering" (D-04).
//!
//! # One scan, reused
//!
//! [`scan_full_alert_corpus`] is the only place this module calls `crates/core::query_alerts`
//! with an unbounded, all-matching filter — every other computation in [`list_handler`] (the
//! displayed page, the facet counts, the withdrawn-hidden count, the noise-floor line) runs
//! against that ONE already-parsed `Vec<AlertRecord>`, never a second `graph_events` scan. The
//! only way to get an unfiltered, unpaged `Vec<AlertRecord>` through `crates/core`'s public API
//! (`parse_alert_row` itself is `pub(crate)` there) is to ask `query_alerts` for "everything,
//! unlimited" and reuse its `AlertPage.alerts` — [`select_alerts`]/[`alert_facets`] are pure and
//! DB-free, so every further slice of that one scan is free.
//!
//! # D-01 — the induced subgraph is read from the CSR snapshot, never re-detected
//!
//! [`resolve_induced_subgraph`] draws exactly what the hot graph says *now* among an alert's
//! participants — the same scope discipline `entity/neighbourhood.rs`'s own doc comment states for
//! the opposite reason (that route wants `identity_links` too; this one deliberately does not,
//! T-46-06-04). A participant id that no longer resolves to a CSR node is skipped, logged at
//! `debug!`, and never fails the whole alert.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::response::Json;
use axum::Extension;
use petgraph::visit::EdgeRef;

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_core::persistence::detector_runs::{list_detector_runs, DetectorRunRecord};
use urban_integrity_core::{
    alert_facets, classify_risk, enrich_alert_participants, get_alert_by_id, query_alerts,
    AlertFacetGroups, AlertFilter, AlertRecord, AlertSort, RiskLevel, WithdrawOutcome,
};
use urban_integrity_detector::alert::result::{MotifKind, MotifType};
use urban_integrity_workbench_dto::caps;
use urban_integrity_workbench_dto::{
    AlertDetailView, AlertFacetOptionView, AlertFacetsView, AlertPageView, AlertSummaryView,
    EntityRef, ImdStateView, InducedSubgraphView, PathEdgeView, PathNodeView,
    PopulationNoiseFloorView, RunAttributionView, ScoreBandView, WithdrawOutcomeView,
    WithdrawnMarkerView,
};

use crate::boot::WorkbenchState;
use crate::entity::{bad_request, internal, not_found, ApiError};
use crate::graph::csr_view::edge_type_name;

/// Never scanned twice per request: `select_alerts`/`alert_facets` are pure, so a single
/// unfiltered, unpaged scan feeds the display page, the facets, the withdrawn-hidden count and
/// the noise-floor line (see this module's doc comment).
async fn scan_full_alert_corpus(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
) -> Result<Vec<AlertRecord>, surrealdb::Error> {
    let everything = AlertFilter {
        min_score: 0.0,
        max_score: None,
        ward_code: None,
        motif_type: None,
        from_date: None,
        to_date: None,
        limit: 0,
        start: 0,
        sort: AlertSort::ScoreDesc,
        include_withdrawn: true,
    };
    Ok(query_alerts(db, &everything).await?.alerts)
}

/// `AlertRecord.motif_type` -> the `MotifType` it names, or `None` for a string this taxonomy
/// does not recognise (a genuinely corrupt row, or a detector this phase's D-04 migration missed).
/// Callers fall back to [`MotifKind::Structural`] on `None` rather than panicking — the generic
/// induced-subgraph path renderer is the safer default for an unknown shape.
fn resolve_motif_type(wire: &str) -> Option<MotifType> {
    MotifType::ALL.into_iter().find(|m| m.wire_str() == wire)
}

/// `queries::classify_risk`'s 4 thresholds, remapped onto the wire's 4-variant naming —
/// `RiskLevel::Low` is `ScoreBandView::BelowMedium` on the wire, never a 5th band invented here.
fn score_band(score: f64) -> ScoreBandView {
    match classify_risk(score) {
        RiskLevel::Critical => ScoreBandView::Critical,
        RiskLevel::High => ScoreBandView::High,
        RiskLevel::Medium => ScoreBandView::Medium,
        RiskLevel::Low => ScoreBandView::BelowMedium,
    }
}

/// D-07's five states. `NotApplicable` for a `MotifKind::SingleSubject` alert (no
/// `ScoreBreakdown` concept exists for those two detectors); `NotRecorded` when the alert predates
/// run-id stamping (D-05) or its run no longer appears in history; otherwise `score_breakdown` is
/// parsed and classified against the producing run's own `imd_loaded` flag.
fn imd_state(
    motif_kind: MotifKind,
    record: &AlertRecord,
    all_runs: &[DetectorRunRecord],
) -> ImdStateView {
    if motif_kind == MotifKind::SingleSubject {
        return ImdStateView::NotApplicable;
    }
    let Some(run_id) = record.first_seen_run_id.as_deref() else {
        return ImdStateView::NotRecorded;
    };
    let Some(run) = all_runs.iter().find(|r| r.run_id == run_id) else {
        return ImdStateView::NotRecorded;
    };
    let result: serde_json::Value =
        serde_json::from_str(&record.result_json).unwrap_or(serde_json::Value::Null);
    let breakdown = &result["score_breakdown"];
    let m_env = breakdown["m_env"].as_f64().unwrap_or(1.0);
    let imd_loaded = run.imd_loaded.unwrap_or(false);
    match (breakdown["imd_rank"].as_u64(), imd_loaded) {
        (Some(rank), true) => ImdStateView::Scored {
            imd_rank: rank as u32,
            m_env,
        },
        (None, true) => ImdStateView::WardNotCovered {
            imd_csv_path: run.imd_csv_path.clone().unwrap_or_default(),
            m_env,
        },
        (_, false) => ImdStateView::NoIndexLoaded { m_env },
    }
}

/// `chrono::DateTime::from_timestamp_millis`, degrading to `None` on an unparseable value rather
/// than a fabricated epoch — mirrors `parse_alert_row`'s own fallback discipline one layer up.
fn ms_to_rfc3339(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

/// D-05/D-06: which run first produced this alert, which run most recently re-confirmed it, and
/// how many of the motif type's own runs have happened since (staleness, legible without a second
/// query).
fn run_attribution(record: &AlertRecord, all_runs: &[DetectorRunRecord]) -> RunAttributionView {
    let find = |run_id: &str| all_runs.iter().find(|r| r.run_id == run_id);
    let first = record.first_seen_run_id.as_deref().and_then(find);
    let last = record.last_seen_run_id.as_deref().and_then(find);

    let runs_since_last_seen = last.and_then(|last_run| {
        let count = all_runs
            .iter()
            .filter(|r| r.motif_types.iter().any(|m| m == &record.motif_type))
            .filter(|r| r.started_at_ms > last_run.started_at_ms)
            .count();
        if count == 0 {
            None
        } else {
            Some(count as u32)
        }
    });

    RunAttributionView {
        first_seen_run_id: record.first_seen_run_id.clone(),
        first_seen_at: first.and_then(|r| ms_to_rfc3339(r.started_at_ms)),
        last_seen_run_id: record.last_seen_run_id.clone(),
        last_seen_at: last.and_then(|r| ms_to_rfc3339(r.started_at_ms)),
        runs_since_last_seen,
    }
}

/// D-11's server-measured noise-floor line. Every motif type's floor is a live computation, no
/// per-type special case: `SingleSubject` types are trivially always at their own floor of 1
/// (their only possible participant count); `Structural` types' floor is the minimum
/// `participants.len()` actually observed for that type in `records` — never a hardcoded literal
/// (`cluster.rs`'s `min_bidders = 3` is a point-in-time detector config, not a constant this view
/// may bake in). Reports whichever motif type has the largest COUNT of alerts sitting at its own
/// floor, ties broken by `MotifType::ALL`'s declaration order (deterministic, never arbitrary
/// `HashMap` iteration order).
fn compute_noise_floor(records: &[AlertRecord]) -> PopulationNoiseFloorView {
    let total = records.len();
    let mut best: Option<(MotifType, usize, usize)> = None;

    for motif in MotifType::ALL {
        let wire = motif.wire_str();
        let matching: Vec<&AlertRecord> = records.iter().filter(|r| r.motif_type == wire).collect();
        if matching.is_empty() {
            continue;
        }
        let floor_participant_count = match motif.motif_kind() {
            MotifKind::SingleSubject => 1,
            MotifKind::Structural => matching
                .iter()
                .map(|r| r.participants.len())
                .min()
                .unwrap_or(0),
        };
        let floor_count = matching
            .iter()
            .filter(|r| r.participants.len() == floor_participant_count)
            .count();

        let is_better = match &best {
            None => true,
            Some((_, best_count, _)) => floor_count > *best_count,
        };
        if is_better {
            best = Some((motif, floor_count, floor_participant_count));
        }
    }

    match best {
        Some((motif, floor_count, floor_participant_count)) => PopulationNoiseFloorView {
            total,
            floor_motif_type: motif.wire_str().to_string(),
            floor_detector_display_name: motif.display_name().to_string(),
            floor_count,
            floor_percent: if total == 0 {
                0.0
            } else {
                (floor_count as f64 / total as f64) * 100.0
            },
            floor_participant_count,
        },
        None => PopulationNoiseFloorView {
            total,
            floor_motif_type: String::new(),
            floor_detector_display_name: String::new(),
            floor_count: 0,
            floor_percent: 0.0,
            floor_participant_count: 0,
        },
    }
}

/// D-13: all 8 canonical detector types always appear, `count` from the facet groups' generic
/// string-keyed map — an entry with zero live matches (e.g. `NepotismBridge`) still renders,
/// visible before it is clicked.
fn build_detector_facets(facet_groups: &AlertFacetGroups) -> Vec<AlertFacetOptionView> {
    MotifType::ALL
        .into_iter()
        .map(|motif| {
            let wire = motif.wire_str();
            AlertFacetOptionView {
                value: wire.to_string(),
                label: motif.display_name().to_string(),
                count: facet_groups.detector_counts.get(wire).copied().unwrap_or(0),
            }
        })
        .collect()
}

/// D-13's ward facet: no name source exists for a ward code in this crate (the ONS
/// ward->local-authority name table is deferred, per `queries.rs::LpaPresence`'s own doc
/// comment), so the code is its own label — the same "no name, so no name is claimed" discipline
/// `lpa_name: None` already applies one layer down.
fn build_ward_facets(facet_groups: &AlertFacetGroups) -> Vec<AlertFacetOptionView> {
    facet_groups
        .ward_counts
        .iter()
        .map(|(code, count)| AlertFacetOptionView {
            value: code.clone(),
            label: code.clone(),
            count: *count,
        })
        .collect()
}

/// D-10: how many additional alerts `filter` would match if `include_withdrawn` were `true`,
/// over an already-scanned corpus. The ONE place this count is computed — [`list_handler`] and
/// [`withdraw_handler`] both call this rather than each re-deriving it, so the two counts cannot
/// drift (D-09's discipline applied to a read, not just the withdraw write itself).
fn withdrawn_hidden_count(all_records: &[AlertRecord], filter: &AlertFilter) -> usize {
    if filter.include_withdrawn {
        return 0;
    }
    let visible =
        urban_integrity_core::persistence::queries::select_alerts(all_records.to_vec(), filter)
            .total_matched;
    let with_withdrawn = urban_integrity_core::persistence::queries::select_alerts(
        all_records.to_vec(),
        &AlertFilter {
            include_withdrawn: true,
            ..filter.clone()
        },
    )
    .total_matched;
    with_withdrawn.saturating_sub(visible)
}

/// The row/detail-shared mapping from a raw [`AlertRecord`] to its wire [`AlertSummaryView`] —
/// the ONE place `score_band`/`imd`/`run` are derived, so a row and its detail page cannot
/// disagree about them (plan 46-06 Task 2). `chip_names` is pre-resolved by the caller (a
/// single batched [`enrich_alert_participants`] call, never one per row) so this function itself
/// stays synchronous and DB-free.
fn build_summary(
    handle: &GraphCoordinatorHandle,
    state: &WorkbenchState,
    record: &AlertRecord,
    all_runs: &[DetectorRunRecord],
    chip_names: &HashMap<String, String>,
    path_node_budget: usize,
    chip_cap: usize,
) -> AlertSummaryView {
    let motif = resolve_motif_type(&record.motif_type);
    let motif_kind = motif
        .as_ref()
        .map(MotifType::motif_kind)
        .unwrap_or(MotifKind::Structural);
    let display_name = motif
        .as_ref()
        .map(|m| m.display_name().to_string())
        .unwrap_or_else(|| record.motif_type.clone());

    let withdrawn = match (&record.withdrawn_at, &record.withdrawn_reason) {
        (Some(at), Some(reason)) => Some(WithdrawnMarkerView {
            withdrawn_at: at.clone(),
            withdrawn_reason: reason.clone(),
        }),
        _ => None,
    };

    let chip_len = record.participants.len().min(chip_cap);
    let participant_chips: Vec<EntityRef> = record.participants[..chip_len]
        .iter()
        .map(|id| {
            let name = chip_names.get(id).cloned().unwrap_or_else(|| id.clone());
            EntityRef {
                id: id.clone(),
                name,
                kind: "Entity".to_string(),
            }
        })
        .collect();

    let path = resolve_induced_subgraph(
        handle,
        state,
        motif_kind,
        &record.participants,
        path_node_budget,
    );

    AlertSummaryView {
        id: record.id.clone(),
        motif_type: record.motif_type.clone(),
        detector_display_name: display_name,
        score: record.score,
        score_band: score_band(record.score),
        imd: imd_state(motif_kind, record, all_runs),
        ward_code: record.ward_code.clone(),
        detected_at: record.detected_at.clone(),
        withdrawn,
        run: run_attribution(record, all_runs),
        participant_total: record.participants.len(),
        participant_chips,
        path,
    }
}

/// D-01/D-02/D-03: the induced subgraph over `participant_ids`, resolved live from the CSR
/// snapshot — a plain, host-testable function (no Axum extractor in its signature).
///
/// `MotifKind::SingleSubject` is a property of the DETECTOR, not a data-driven fallback: it
/// always renders [`InducedSubgraphView::SingleParticipant`] regardless of how many ids happen
/// to be in `participant_ids` (plan 46-03's `MotifKind` distinction exists for exactly this
/// dispatch). A single-element `participant_ids` gets the same treatment for any motif kind —
/// there is no path to draw with one node either way.
///
/// Edges are read from `snapshot.graph.edges(node)` — the forward-only outgoing CSR list,
/// iterated once per scope node — deduplicated as `(min(a,b), max(a,b), edge_type_name)` triples
/// so an edge is never emitted twice by also walking the reverse index (`csr_view.rs`'s
/// `build_scoped_adjacency` is the reference for this technique, without its edge-type filter:
/// this resolver wants every edge type). A participant id that does not resolve to a CSR node is
/// skipped (`debug!`), never failing the whole alert — the same graceful-degradation discipline
/// `csr_view.rs`'s own callers apply.
pub fn resolve_induced_subgraph(
    handle: &GraphCoordinatorHandle,
    state: &WorkbenchState,
    motif_kind: MotifKind,
    participant_ids: &[String],
    node_budget: usize,
) -> InducedSubgraphView {
    if motif_kind == MotifKind::SingleSubject || participant_ids.len() == 1 {
        return single_participant_view(state, participant_ids);
    }

    let snapshot = handle.snapshot.load();

    let mut resolved: Vec<u32> = Vec::new();
    for id in participant_ids {
        match state.label_index.index_of(id) {
            Some(idx) => resolved.push(idx),
            None => {
                tracing::debug!(
                    participant_id = %id,
                    "alert participant did not resolve to a CSR node index — skipped from \
                     induced-subgraph resolution"
                );
            }
        }
    }
    let scope: HashSet<u32> = resolved.into_iter().collect();

    let mut seen: HashSet<(u32, u32, &'static str)> = HashSet::new();
    let mut degree: HashMap<u32, u32> = HashMap::new();
    let mut kept_edges: Vec<(u32, u32, &'static str)> = Vec::new();

    for &node in &scope {
        for edge_ref in snapshot.graph.edges(node) {
            let target = edge_ref.target();
            if node == target || !scope.contains(&target) {
                continue;
            }
            let type_name = edge_type_name(edge_ref.weight());
            let key = (node.min(target), node.max(target), type_name);
            if seen.insert(key) {
                kept_edges.push(key);
                *degree.entry(node).or_insert(0) += 1;
                *degree.entry(target).or_insert(0) += 1;
            }
        }
    }

    // (entity, degree, csr index) — degree descending, entity id ascending (total order).
    let mut ranked: Vec<(EntityRef, u32, u32)> = scope
        .iter()
        .map(|&idx| {
            let entity = state
                .label_index
                .entity_ref_at(idx)
                .unwrap_or_else(|| EntityRef {
                    id: idx.to_string(),
                    name: format!("node {idx}"),
                    kind: "Entity".to_string(),
                });
            let degree_in_subgraph = degree.get(&idx).copied().unwrap_or(0);
            (entity, degree_in_subgraph, idx)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
    ranked.truncate(node_budget);

    let surviving_ids: HashMap<u32, String> = ranked
        .iter()
        .map(|(entity, _, idx)| (*idx, entity.id.clone()))
        .collect();

    let nodes: Vec<PathNodeView> = ranked
        .into_iter()
        .map(|(entity, degree_in_subgraph, _)| PathNodeView {
            entity,
            degree_in_subgraph,
        })
        .collect();

    let edges: Vec<PathEdgeView> = kept_edges
        .into_iter()
        .filter(|(a, b, _)| surviving_ids.contains_key(a) && surviving_ids.contains_key(b))
        .map(|(a, b, type_name)| PathEdgeView {
            from_id: surviving_ids.get(&a).cloned().unwrap_or_default(),
            to_id: surviving_ids.get(&b).cloned().unwrap_or_default(),
            edge_type: type_name.to_string(),
        })
        .collect();

    let participant_total = participant_ids.len();

    if edges.is_empty() && participant_total >= 2 {
        return InducedSubgraphView::NoEdges {
            participant_total,
            snapshot_generated_at: state.replayed_at.to_rfc3339(),
        };
    }

    InducedSubgraphView::Drawable {
        drawn_total: nodes.len(),
        nodes,
        edges,
        participant_total,
    }
}

/// The `MotifKind::SingleSubject`/one-participant case — no path to draw, ever. Falls back
/// gracefully (never panics) when the single id does not resolve via the label index, or when
/// `participant_ids` is itself empty (a pre-participants-wiring legacy row).
fn single_participant_view(
    state: &WorkbenchState,
    participant_ids: &[String],
) -> InducedSubgraphView {
    let entity = participant_ids
        .first()
        .map(|id| {
            state
                .label_index
                .index_of(id)
                .and_then(|idx| state.label_index.entity_ref_at(idx))
                .unwrap_or_else(|| EntityRef {
                    id: id.clone(),
                    name: id.clone(),
                    kind: "Entity".to_string(),
                })
        })
        .unwrap_or_else(|| EntityRef {
            id: String::new(),
            name: "Unknown participant".to_string(),
            kind: "Entity".to_string(),
        });
    InducedSubgraphView::SingleParticipant { entity }
}

const DEFAULT_ALERT_LIST_LIMIT: usize = 50;
const MAX_ALERT_LIST_LIMIT: usize = 1000;

/// Query parameters for [`list_handler`]. D-11: no field defaults to a hidden floor — an absent
/// param means "match everything" for that dimension.
#[derive(Debug, Default, serde::Deserialize)]
pub struct AlertListQuery {
    ward: Option<String>,
    motif: Option<String>,
    min_score: Option<f64>,
    max_score: Option<f64>,
    from: Option<String>,
    to: Option<String>,
    sort: Option<String>,
    start: Option<usize>,
    limit: Option<usize>,
    include_withdrawn: Option<bool>,
}

/// `GET /api/alerts?ward=…&motif=…&min_score=…&max_score=…&from=…&to=…&sort=…&start=…&limit=…&include_withdrawn=…`
///
/// One scan (see this module's doc comment), then every other figure in the response — the paged
/// alerts, the facets, `withdrawn_hidden_count`, the noise-floor line — is a pure slice of it.
pub async fn list_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<AlertListQuery>,
) -> Result<Json<AlertPageView>, ApiError> {
    let limit = params
        .limit
        .unwrap_or(DEFAULT_ALERT_LIST_LIMIT)
        .clamp(1, MAX_ALERT_LIST_LIMIT);
    let start = params.start.unwrap_or(0);
    let sort = match params.sort.as_deref() {
        Some("recency") => AlertSort::RecencyDesc,
        _ => AlertSort::ScoreDesc,
    };
    let filter = AlertFilter {
        min_score: params.min_score.unwrap_or(0.0),
        max_score: params.max_score,
        ward_code: params.ward.clone(),
        motif_type: params.motif.clone(),
        from_date: params.from.clone(),
        to_date: params.to.clone(),
        limit,
        start,
        sort,
        include_withdrawn: params.include_withdrawn.unwrap_or(false),
    };

    let all_records = scan_full_alert_corpus(&state.db)
        .await
        .map_err(|e| internal(format!("alert query failed: {e}")))?;

    let page =
        urban_integrity_core::persistence::queries::select_alerts(all_records.clone(), &filter);
    let facet_groups = alert_facets(&all_records, &filter);
    let hidden_count = withdrawn_hidden_count(&all_records, &filter);
    let population_note = compute_noise_floor(&all_records);

    let all_runs = list_detector_runs(&state.db).await.unwrap_or_default();
    let detector_run_recorded = !all_runs.is_empty();

    let mut chip_ids: HashSet<String> = HashSet::new();
    for record in &page.alerts {
        let cap = record
            .participants
            .len()
            .min(caps::ALERT_ROW_PARTICIPANT_CHIPS);
        chip_ids.extend(record.participants[..cap].iter().cloned());
    }
    let chip_ids: Vec<String> = chip_ids.into_iter().collect();
    let chip_names = enrich_alert_participants(&state.db, &chip_ids)
        .await
        .unwrap_or_default();

    let alerts: Vec<AlertSummaryView> = page
        .alerts
        .iter()
        .map(|record| {
            build_summary(
                &handle,
                &state,
                record,
                &all_runs,
                &chip_names,
                caps::ALERT_PATH_NODE_BUDGET_ROW,
                caps::ALERT_ROW_PARTICIPANT_CHIPS,
            )
        })
        .collect();

    Ok(Json(AlertPageView {
        alerts,
        total_matched: page.total_matched,
        truncated: page.truncated(),
        withdrawn_hidden_count: hidden_count,
        facets: AlertFacetsView {
            detectors: build_detector_facets(&facet_groups),
            wards: build_ward_facets(&facet_groups),
            no_ward_code_count: facet_groups.no_ward_code_count,
        },
        population_note,
        detector_run_recorded,
    }))
}

/// Query parameters for [`detail_handler`] — empty; the id comes from the path.
#[derive(Debug, Default, serde::Deserialize)]
pub struct AlertDetailQuery {}

/// `GET /api/alert/:id` — the full detail view (D-01/D-02/D-03), or a clean 404 (T-46-01).
pub async fn detail_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
    Path(id): Path<String>,
) -> Result<Json<AlertDetailView>, ApiError> {
    let record = get_alert_by_id(&state.db, &id)
        .await
        .map_err(|e| internal(format!("alert lookup failed: {e}")))?
        .ok_or_else(|| not_found(format!("No alert with id {id} exists in this analysis.")))?;

    let motif = resolve_motif_type(&record.motif_type);
    let motif_kind = motif
        .map(|m| m.motif_kind())
        .unwrap_or(MotifKind::Structural);

    let all_runs = list_detector_runs(&state.db).await.unwrap_or_default();

    let full_cap = record
        .participants
        .len()
        .min(caps::ALERT_PARTICIPANTS_LIST_CAP);
    let name_map = enrich_alert_participants(&state.db, &record.participants[..full_cap])
        .await
        .unwrap_or_default();

    let summary = build_summary(
        &handle,
        &state,
        &record,
        &all_runs,
        &name_map,
        caps::ALERT_PATH_NODE_BUDGET_ROW,
        caps::ALERT_ROW_PARTICIPANT_CHIPS,
    );

    let path_detail = resolve_induced_subgraph(
        &handle,
        &state,
        motif_kind,
        &record.participants,
        caps::ALERT_PATH_NODE_BUDGET_DETAIL,
    );

    let participants: Vec<EntityRef> = record.participants[..full_cap]
        .iter()
        .map(|id| {
            let name = name_map.get(id).cloned().unwrap_or_else(|| id.clone());
            EntityRef {
                id: id.clone(),
                name,
                kind: "Entity".to_string(),
            }
        })
        .collect();
    let participants_truncated = record.participants.len() > full_cap;

    Ok(Json(AlertDetailView {
        summary,
        path_detail,
        participants,
        participants_truncated,
    }))
}

/// `POST /api/alert/:id/withdraw` body — a reason is mandatory (D-10), matching the CLI's own
/// `--reason`-required arg.
#[derive(Debug, serde::Deserialize)]
pub struct WithdrawRequest {
    reason: String,
}

/// `POST /api/alert/:id/withdraw` — D-08/D-09/D-10. The already-withdrawn guard lives inside
/// `core::withdraw_alert` itself (46-02), so this handler and the CLI's `alerts withdraw` enforce
/// the identical guard by construction; a caller cannot bypass it by skipping a separate
/// pre-check.
pub async fn withdraw_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Path(id): Path<String>,
    Json(body): Json<WithdrawRequest>,
) -> Result<Json<WithdrawOutcomeView>, ApiError> {
    if body.reason.trim().is_empty() {
        return Err(bad_request("A reason is required."));
    }

    let outcome =
        urban_integrity_core::withdraw_alert(&state.db, &id, &body.reason, chrono::Utc::now())
            .await
            .map_err(|e| internal(format!("withdraw failed: {e}")))?;

    let view = match outcome {
        WithdrawOutcome::Withdrawn => {
            let all_records = scan_full_alert_corpus(&state.db)
                .await
                .map_err(|e| internal(format!("alert query failed: {e}")))?;
            let hidden_count = withdrawn_hidden_count(&all_records, &AlertFilter::default());
            WithdrawOutcomeView::Withdrawn { hidden_count }
        }
        WithdrawOutcome::AlreadyWithdrawn { at, reason } => {
            WithdrawOutcomeView::AlreadyWithdrawn { at, reason }
        }
        WithdrawOutcome::NotFound => WithdrawOutcomeView::NotFound,
    };

    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn alert_fixture(id: &str, motif: &str, score: f64, participants: &[&str]) -> AlertRecord {
        AlertRecord {
            id: id.to_string(),
            motif_type: motif.to_string(),
            score,
            result_json: "{}".to_string(),
            detected_at: "2026-09-21T00:00:00+00:00".to_string(),
            ward_code: None,
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: participants.iter().map(|s| s.to_string()).collect(),
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_string(),
            },
        }
    }

    // ── score_band ──────────────────────────────────────────────────────────

    #[test]
    fn score_band_thresholds_match_classify_risk() {
        assert_eq!(score_band(0.95), ScoreBandView::Critical);
        assert_eq!(score_band(0.9), ScoreBandView::Critical);
        assert_eq!(score_band(0.89), ScoreBandView::High);
        assert_eq!(score_band(0.7), ScoreBandView::High);
        assert_eq!(score_band(0.69), ScoreBandView::Medium);
        assert_eq!(score_band(0.5), ScoreBandView::Medium);
        assert_eq!(score_band(0.49), ScoreBandView::BelowMedium);
        assert_eq!(score_band(0.0), ScoreBandView::BelowMedium);
    }

    // ── noise floor (D-11) ──────────────────────────────────────────────────

    #[test]
    fn noise_floor_picks_the_motif_type_with_the_most_alerts_at_its_own_live_measured_floor() {
        let records = vec![
            alert_fixture("c1", "AddressCluster", 0.6, &["p1", "p2", "p3"]),
            alert_fixture("c2", "AddressCluster", 0.6, &["p1", "p2", "p3"]),
            alert_fixture("c3", "AddressCluster", 0.9, &["p1", "p2", "p3", "p4", "p5"]),
            alert_fixture("p1", "PhoenixLoop", 0.5, &["p1", "p2"]),
        ];
        let noise = compute_noise_floor(&records);
        assert_eq!(noise.total, 4);
        assert_eq!(noise.floor_motif_type, "AddressCluster");
        assert_eq!(
            noise.floor_participant_count, 3,
            "the live-measured minimum, not a hardcoded 3"
        );
        assert_eq!(noise.floor_count, 2);
        assert!((noise.floor_percent - 50.0).abs() < 1e-9);
    }

    #[test]
    fn noise_floor_on_an_empty_corpus_is_a_stated_zero_never_a_panic() {
        let noise = compute_noise_floor(&[]);
        assert_eq!(noise.total, 0);
        assert_eq!(noise.floor_count, 0);
        assert_eq!(noise.floor_percent, 0.0);
    }

    #[test]
    fn noise_floor_single_subject_types_are_trivially_always_at_their_floor_of_one() {
        let records = vec![
            alert_fixture("d1", "donation_contract_correlation", 0.7, &["donor-1"]),
            alert_fixture("d2", "donation_contract_correlation", 0.7, &["donor-2"]),
        ];
        let noise = compute_noise_floor(&records);
        assert_eq!(noise.floor_motif_type, "donation_contract_correlation");
        assert_eq!(noise.floor_participant_count, 1);
        assert_eq!(noise.floor_count, 2);
    }

    // ── facets (D-13) ───────────────────────────────────────────────────────

    #[test]
    fn detector_facets_always_list_all_8_motif_types_even_at_zero_count() {
        let records = vec![alert_fixture("a1", "PhoenixLoop", 0.9, &["p1"])];
        let facet_groups = alert_facets(&records, &AlertFilter::default());
        let detectors = build_detector_facets(&facet_groups);

        assert_eq!(
            detectors.len(),
            8,
            "all 8 MotifType::ALL entries must appear"
        );
        let address_cluster = detectors
            .iter()
            .find(|d| d.value == "AddressCluster")
            .expect("AddressCluster must appear even at zero live matches");
        assert_eq!(address_cluster.count, 0);
        let phoenix = detectors
            .iter()
            .find(|d| d.value == "PhoenixLoop")
            .expect("PhoenixLoop must appear");
        assert_eq!(phoenix.count, 1);
    }

    // ── resolve_induced_subgraph (plan 46-06 Task 2 behaviour) ────────────

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

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
            meta: meta(),
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        }
    }

    #[tokio::test]
    async fn drawable_keeps_an_isolated_participant_as_a_degree_zero_node() {
        let nodes = vec![
            (EntityId("a".into()), entity("A")),
            (EntityId("b".into()), entity("B")),
            (EntityId("c".into()), entity("C")),
        ];
        let edges = vec![(EntityId("a".into()), EntityId("b".into()), directs())];
        let (handle, state) =
            crate::graph::fixture::workbench_from_graph(&nodes, &edges, &[]).await;

        let view = resolve_induced_subgraph(
            &handle,
            &state,
            MotifKind::Structural,
            &["a".into(), "b".into(), "c".into()],
            10,
        );

        match view {
            InducedSubgraphView::Drawable {
                nodes,
                edges,
                participant_total,
                drawn_total,
            } => {
                assert_eq!(participant_total, 3);
                assert_eq!(drawn_total, 3);
                assert_eq!(
                    nodes.len(),
                    3,
                    "the isolated participant c must still appear"
                );
                assert_eq!(edges.len(), 1, "exactly one a-b edge");
            }
            other => panic!("expected Drawable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_edges_among_participants_states_the_absence_outright() {
        let nodes = vec![
            (EntityId("a".into()), entity("A")),
            (EntityId("b".into()), entity("B")),
            (EntityId("z".into()), entity("Z")),
        ];
        // a and b both connect only to z, an entity outside the alert's own participant set.
        let edges = vec![
            (EntityId("a".into()), EntityId("z".into()), directs()),
            (EntityId("b".into()), EntityId("z".into()), directs()),
        ];
        let (handle, state) =
            crate::graph::fixture::workbench_from_graph(&nodes, &edges, &[]).await;

        let view = resolve_induced_subgraph(
            &handle,
            &state,
            MotifKind::Structural,
            &["a".into(), "b".into()],
            10,
        );

        match view {
            InducedSubgraphView::NoEdges {
                participant_total, ..
            } => assert_eq!(participant_total, 2),
            other => panic!("expected NoEdges, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exactly_one_participant_is_a_single_participant_view() {
        let nodes = vec![(EntityId("a".into()), entity("A"))];
        let (handle, state) = crate::graph::fixture::workbench_from_graph(&nodes, &[], &[]).await;

        let view =
            resolve_induced_subgraph(&handle, &state, MotifKind::Structural, &["a".into()], 10);

        match view {
            InducedSubgraphView::SingleParticipant { entity } => assert_eq!(entity.id, "a"),
            other => panic!("expected SingleParticipant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn single_subject_motif_kind_always_wins_over_multiple_listed_participants() {
        let nodes = vec![
            (EntityId("a".into()), entity("A")),
            (EntityId("b".into()), entity("B")),
        ];
        let edges = vec![(EntityId("a".into()), EntityId("b".into()), directs())];
        let (handle, state) =
            crate::graph::fixture::workbench_from_graph(&nodes, &edges, &[]).await;

        let view = resolve_induced_subgraph(
            &handle,
            &state,
            MotifKind::SingleSubject,
            &["a".into(), "b".into()],
            10,
        );

        match view {
            InducedSubgraphView::SingleParticipant { entity } => {
                assert_eq!(
                    entity.id, "a",
                    "the detector's own single subject, never a-and-b's path"
                );
            }
            other => panic!("expected SingleParticipant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn node_order_is_degree_descending_then_entity_id_ascending_on_a_tie() {
        // hub -Directs-> leaf-y, hub -Directs-> leaf-x: hub degree 2, leaf-x/leaf-y tied at 1.
        let nodes = vec![
            (EntityId("hub".into()), entity("Hub")),
            (EntityId("leaf-y".into()), entity("LeafY")),
            (EntityId("leaf-x".into()), entity("LeafX")),
        ];
        let edges = vec![
            (EntityId("hub".into()), EntityId("leaf-y".into()), directs()),
            (EntityId("hub".into()), EntityId("leaf-x".into()), directs()),
        ];
        let (handle, state) =
            crate::graph::fixture::workbench_from_graph(&nodes, &edges, &[]).await;

        let view = resolve_induced_subgraph(
            &handle,
            &state,
            MotifKind::Structural,
            &["hub".into(), "leaf-y".into(), "leaf-x".into()],
            10,
        );

        match view {
            InducedSubgraphView::Drawable { nodes, .. } => {
                let ids: Vec<&str> = nodes.iter().map(|n| n.entity.id.as_str()).collect();
                assert_eq!(
                    ids,
                    vec!["hub", "leaf-x", "leaf-y"],
                    "hub first (degree 2), then the tied leaves by id ascending"
                );
            }
            other => panic!("expected Drawable, got {other:?}"),
        }
    }

    /// DB-gated integration variant (plan 46-06 Task 2 acceptance criteria): proves the
    /// resolver reads a real, TestNs-backed connection alongside a real baked CSR snapshot —
    /// `resolve_induced_subgraph` itself never touches `state.db`, so this is the same behaviour
    /// exercised with a genuinely authenticated handle present, not a second algorithm.
    #[tokio::test]
    async fn db_induced_subgraph_resolves_against_a_real_baked_snapshot() {
        if !urban_integrity_test_support::db_tests_enabled() {
            return;
        }
        let ns = urban_integrity_test_support::TestNs::connect("alert_induced_subgraph").await;
        let nodes = vec![
            (EntityId("a".into()), entity("A")),
            (EntityId("b".into()), entity("B")),
        ];
        let edges = vec![(EntityId("a".into()), EntityId("b".into()), directs())];
        let (handle, state) =
            crate::graph::fixture::workbench_from_graph_with_db(&nodes, &edges, &[], ns.arc())
                .await;

        let view = resolve_induced_subgraph(
            &handle,
            &state,
            MotifKind::Structural,
            &["a".into(), "b".into()],
            10,
        );

        match view {
            InducedSubgraphView::Drawable { edges, .. } => assert_eq!(edges.len(), 1),
            other => panic!("expected Drawable, got {other:?}"),
        }
    }

    /// T-46-01: an unknown `:id` must resolve to a clean not-found `ApiError`, never a panic —
    /// asserted by calling `detail_handler` directly against a real (empty) TestNs-backed
    /// database, so the assertion exercises the genuine `get_alert_by_id -> None -> not_found`
    /// path rather than merely an unconnected-handle error. Ungated, this test still "passes"
    /// (0 assertions) per this crate's existing `db_tests_enabled()` skip convention.
    #[tokio::test]
    async fn alert_detail_unknown_id_returns_not_found_never_panics() {
        if !urban_integrity_test_support::db_tests_enabled() {
            return;
        }
        let ns = urban_integrity_test_support::TestNs::connect("alert_detail_unknown").await;
        let (handle, state) =
            crate::graph::fixture::workbench_from_graph_with_db(&[], &[], &[], ns.arc()).await;

        let result = detail_handler(
            Extension(handle),
            Extension(state),
            Path("does-not-exist".to_string()),
        )
        .await;

        let err = result.expect_err("an unknown id must not resolve to Ok");
        assert_eq!(
            err.0,
            axum::http::StatusCode::NOT_FOUND,
            "must be a 404-shaped ApiError, not a panic or a 500"
        );
    }

    // ── withdraw_handler (plan 46-06 Task 3) ──────────────────────────────

    #[tokio::test]
    async fn withdraw_handler_rejects_empty_reason_before_any_db_call() {
        let (_handle, state) = crate::graph::fixture::workbench_from_graph(&[], &[], &[]).await;

        let result = withdraw_handler(
            Extension(state),
            Path("whatever".to_string()),
            Json(WithdrawRequest {
                reason: "   ".to_string(),
            }),
        )
        .await;

        assert!(
            result.is_err(),
            "an empty/whitespace reason must be rejected before withdraw_alert is ever called \
             (state.db is Surreal::init() — uninitialised — so reaching it here would fail loudly)"
        );
    }

    /// D-09: a CLI-path withdrawal (`core::withdraw_alert` called directly, exactly as
    /// `crates/cli/src/commands/alerts.rs`'s `Withdraw` arm does) and an API-path withdrawal
    /// (`withdraw_handler` called directly, not over HTTP) must leave byte-identical
    /// `result_json` state modulo the `withdrawn_at` timestamp. Non-vacuous: both rows' presence
    /// is counted BEFORE either withdrawal runs.
    #[tokio::test]
    async fn withdraw_parity_cli_and_api_leave_byte_identical_state() {
        if !urban_integrity_test_support::db_tests_enabled() {
            return;
        }
        let ns = urban_integrity_test_support::TestNs::connect("alert_withdraw_parity").await;

        for id in ["parity-a", "parity-b"] {
            let payload = serde_json::json!({
                "motif_type": "PhoenixLoop",
                "risk_score": 0.85,
                "participants": ["GB-COH:X0699001"],
            })
            .to_string();
            ns.query(
                "CREATE graph_events SET kind = 'MotifDetected', event_id = $e, \
                 timestamp = $t, payload = { result_json: $r }",
            )
            .bind(("e", id.to_string()))
            .bind(("t", 1_700_000_000_000_i64))
            .bind(("r", payload))
            .await
            .expect("seed alert");
        }

        // Non-vacuity (D-09, this project's own phase 44 verification lesson): count BOTH rows
        // before either withdrawal runs, so a test comparing two empty results cannot pass.
        let before: Vec<serde_json::Value> = ns
            .query("SELECT * FROM graph_events WHERE event_id IN ['parity-a', 'parity-b']")
            .await
            .expect("select before")
            .take(0)
            .expect("take before");
        assert_eq!(
            before.len(),
            2,
            "both seeded rows must exist before either withdrawal"
        );

        let reason = "confirmed false positive";
        let at = chrono::Utc::now();

        let cli_outcome = urban_integrity_core::withdraw_alert(&ns, "parity-a", reason, at)
            .await
            .expect("CLI-path withdraw_alert must succeed");
        assert_eq!(cli_outcome, WithdrawOutcome::Withdrawn);

        let (_handle, state) =
            crate::graph::fixture::workbench_from_graph_with_db(&[], &[], &[], ns.arc()).await;
        let api_response = withdraw_handler(
            Extension(state),
            Path("parity-b".to_string()),
            Json(WithdrawRequest {
                reason: reason.to_string(),
            }),
        )
        .await
        .expect("API-path withdraw_handler must succeed");
        match api_response.0 {
            WithdrawOutcomeView::Withdrawn { .. } => {}
            other => panic!("expected Withdrawn, got {other:?}"),
        }

        let after: Vec<serde_json::Value> = ns
            .query("SELECT * FROM graph_events WHERE event_id IN ['parity-a', 'parity-b']")
            .await
            .expect("select after")
            .take(0)
            .expect("take after");
        assert_eq!(after.len(), 2, "withdrawal marks, it never deletes");

        let row_result = |rows: &[serde_json::Value], id: &str| -> serde_json::Value {
            let row = rows
                .iter()
                .find(|r| r["event_id"] == id)
                .unwrap_or_else(|| panic!("row {id} must exist"));
            let raw = row["payload"]["result_json"]
                .as_str()
                .expect("result_json string");
            serde_json::from_str(raw).expect("result_json must be valid JSON")
        };
        let mut a_result = row_result(&after, "parity-a");
        let mut b_result = row_result(&after, "parity-b");

        assert!(
            a_result["withdrawn_at"].is_string(),
            "CLI path must stamp withdrawn_at"
        );
        assert!(
            b_result["withdrawn_at"].is_string(),
            "API path must stamp withdrawn_at"
        );
        assert_eq!(
            a_result["withdrawn_reason"], b_result["withdrawn_reason"],
            "same reason given to both — must match exactly"
        );

        // Byte-identical modulo the timestamp (each call ran chrono::Utc::now() separately).
        a_result["withdrawn_at"] = serde_json::Value::Null;
        b_result["withdrawn_at"] = serde_json::Value::Null;
        assert_eq!(
            a_result, b_result,
            "CLI and API withdrawal must leave byte-identical result_json modulo the timestamp"
        );
    }
}
