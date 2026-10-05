//! Saved-investigation CRUD (PIV-05) — `/api/investigations` and `/api/investigation*`.
//!
//! # Every write goes through `crates/core::persistence::saved_investigations`
//!
//! No SurrealQL here, same as everywhere else on this surface, and for a sharper reason than
//! tidiness: 45-07's write layer is where the fail-loud discipline lives. A rename that matched no
//! row is an `Err` there, not a quiet `Ok`; a pin against a missing investigation fails on type
//! coercion inside one statement rather than leaving a dangling member; a re-pin preserves
//! `pinned_at` and an existing note. Every one of those properties would have to be re-created,
//! and could be re-created *wrongly*, by a handler that wrote its own statements.
//!
//! This project has twice shipped a write path that reported success over nothing — the ICIJ
//! adapter logged 814k rows written against an empty table, and the PPD enrich reported
//! "2795151 written" against 0 staged rows. So the rule these handlers keep is narrow and
//! absolute: **a response that says something was saved is a response whose write returned a
//! row.** An `Err` from the layer below becomes an error status, never a 200 with a cheerful body.
//!
//! # Status codes
//!
//! `404` means the investigation (or the member) is not there; `400` means the request could not
//! be acted on as written; `500` is a read or write that failed for any other reason. A missing
//! investigation is established by reading it first, so a 404 is a fact rather than an inference
//! from an error string.

use std::sync::Arc;

use axum::extract::Query;
use axum::response::Json;
use axum::Extension;

use urban_integrity_core::persistence::saved_investigations::{
    create_investigation, delete_investigation, get_investigation, list_investigations, list_pins,
    pin_member, rename_investigation, set_investigation_notes, set_pin_note, unpin_member,
    InvestigationSummary, PinOrigin, PinTarget, SavedInvestigation, SavedPin,
};
use urban_integrity_workbench_dto::investigation::{
    CreateInvestigationRequest, DeleteResultView, InvestigationDetailView,
    InvestigationSummaryView, PinNoteRequest, PinOriginView, PinRequest, PinTargetView,
    SavedInvestigationView, SavedPinView, UnpinRequest, UnpinResultView,
    UpdateInvestigationRequest,
};

use crate::boot::WorkbenchState;
use crate::entity::{bad_request, internal, not_found, ApiError};

/// Identifies one investigation. A query parameter rather than a path segment so every route on
/// this surface reads the same way (`?id=`), matching `/api/graph/node` and `/api/entity/profile`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct InvestigationQuery {
    id: Option<String>,
}

fn required_id(params: &InvestigationQuery) -> Result<&str, ApiError> {
    params
        .id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_request("`id` is required"))
}

fn investigation_view(i: &SavedInvestigation) -> SavedInvestigationView {
    SavedInvestigationView {
        id: i.id.clone(),
        name: i.name.clone(),
        analysis_db: i.analysis_db.clone(),
        notes: i.notes.clone(),
        schema_version: i.schema_version,
        created_at: i.created_at.to_rfc3339(),
        updated_at: i.updated_at.to_rfc3339(),
    }
}

fn target_view(t: &PinTarget) -> PinTargetView {
    match t {
        PinTarget::Entity { entity_id } => PinTargetView::Entity {
            entity_id: entity_id.clone(),
        },
        PinTarget::Edge { from, to, relation } => PinTargetView::Edge {
            from: from.clone(),
            to: to.clone(),
            relation: relation.clone(),
        },
    }
}

fn target_from_view(t: &PinTargetView) -> PinTarget {
    match t {
        PinTargetView::Entity { entity_id } => PinTarget::Entity {
            entity_id: entity_id.clone(),
        },
        PinTargetView::Edge { from, to, relation } => PinTarget::Edge {
            from: from.clone(),
            to: to.clone(),
            relation: relation.clone(),
        },
    }
}

fn origin_view(o: PinOrigin) -> PinOriginView {
    match o {
        PinOrigin::Profile => PinOriginView::Profile,
        PinOrigin::Canvas => PinOriginView::Canvas,
        PinOrigin::Subgraph => PinOriginView::Subgraph,
        PinOrigin::Alert => PinOriginView::Alert,
    }
}

fn origin_from_view(o: PinOriginView) -> PinOrigin {
    match o {
        PinOriginView::Profile => PinOrigin::Profile,
        PinOriginView::Canvas => PinOrigin::Canvas,
        PinOriginView::Subgraph => PinOrigin::Subgraph,
        PinOriginView::Alert => PinOrigin::Alert,
    }
}

fn pin_view(p: &SavedPin) -> SavedPinView {
    SavedPinView {
        id: p.id.clone(),
        target: target_view(&p.target),
        note: p.note.clone(),
        origin: origin_view(p.origin),
        pinned_at: p.pinned_at.to_rfc3339(),
        updated_at: p.updated_at.to_rfc3339(),
    }
}

fn summary_view(s: &InvestigationSummary) -> InvestigationSummaryView {
    InvestigationSummaryView {
        investigation: investigation_view(&s.investigation),
        entity_count: s.entity_count as u64,
        edge_count: s.edge_count as u64,
    }
}

/// Read the investigation, or produce the 404 every mutating route below starts with.
async fn require_investigation(
    state: &WorkbenchState,
    id: &str,
) -> Result<SavedInvestigation, ApiError> {
    get_investigation(&state.db, id)
        .await
        .map_err(|e| internal(format!("investigation read failed: {e}")))?
        .ok_or_else(|| {
            not_found(format!(
                "No saved investigation {id} exists in this analysis."
            ))
        })
}

/// `GET /api/investigations` — every investigation in this analysis, most recently modified first.
pub async fn list_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Result<Json<Vec<InvestigationSummaryView>>, ApiError> {
    let rows = list_investigations(&state.db)
        .await
        .map_err(|e| internal(format!("investigation list failed: {e}")))?;
    Ok(Json(rows.iter().map(summary_view).collect()))
}

/// `POST /api/investigations` — create one.
///
/// A blank or over-long name is rejected by `create_investigation`'s own validation, which is why
/// that error maps to `400` here: it is a statement about the request, not a server fault.
pub async fn create_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Json(body): Json<CreateInvestigationRequest>,
) -> Result<Json<SavedInvestigationView>, ApiError> {
    let created = create_investigation(&state.db, &body.name, &body.notes)
        .await
        .map_err(|e| bad_request(format!("investigation not created: {e}")))?;
    Ok(Json(investigation_view(&created)))
}

/// `GET /api/investigation?id=…` — one investigation with every member and note.
///
/// This is PIV-05's "reopening restores the pinned set and every note" in one request. The counts
/// are recomputed from the pins rather than carried from the list, so a detail view cannot state a
/// count its own member list contradicts.
pub async fn get_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
) -> Result<Json<InvestigationDetailView>, ApiError> {
    let id = required_id(&params)?;
    let investigation = require_investigation(&state, id).await?;
    let pins = list_pins(&state.db, id)
        .await
        .map_err(|e| internal(format!("investigation members read failed: {e}")))?;

    let entity_count = pins
        .iter()
        .filter(|p| matches!(p.target, PinTarget::Entity { .. }))
        .count() as u64;
    let edge_count = pins.len() as u64 - entity_count;

    Ok(Json(InvestigationDetailView {
        investigation: investigation_view(&investigation),
        entity_count,
        edge_count,
        pins: pins.iter().map(pin_view).collect(),
    }))
}

/// `PATCH /api/investigation?id=…` — rename, re-note, or both.
///
/// A body asking for neither is a `400`, not a silent success: the notes editor saves on blur and
/// shows a save state, and "saved" over a request that changed nothing is exactly the false
/// reassurance this surface must not give.
pub async fn update_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
    Json(body): Json<UpdateInvestigationRequest>,
) -> Result<Json<SavedInvestigationView>, ApiError> {
    let id = required_id(&params)?;
    if body.name.is_none() && body.notes.is_none() {
        return Err(bad_request(
            "an update must set `name`, `notes`, or both — a request that changes nothing must \
             not report that something was saved",
        ));
    }
    let mut current = require_investigation(&state, id).await?;

    if let Some(name) = &body.name {
        current = rename_investigation(&state.db, id, name)
            .await
            .map_err(|e| bad_request(format!("rename failed: {e}")))?;
    }
    if let Some(notes) = &body.notes {
        current = set_investigation_notes(&state.db, id, notes)
            .await
            .map_err(|e| internal(format!("notes not saved: {e}")))?;
    }
    Ok(Json(investigation_view(&current)))
}

/// `DELETE /api/investigation?id=…` — the one destructive action this phase adds.
///
/// Returns how many members went with it, because "deleted" over an investigation the analyst
/// thought held forty pinned entities should be able to say forty.
pub async fn delete_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
) -> Result<Json<DeleteResultView>, ApiError> {
    let id = required_id(&params)?;
    match delete_investigation(&state.db, id)
        .await
        .map_err(|e| internal(format!("delete failed: {e}")))?
    {
        Some(removed) => Ok(Json(DeleteResultView {
            removed_members: removed as u64,
        })),
        None => Err(not_found(format!(
            "No saved investigation {id} exists in this analysis."
        ))),
    }
}

/// `POST /api/investigation/pin?id=…` — pin an entity or an edge. Idempotent (PIV-05).
pub async fn pin_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
    Json(body): Json<PinRequest>,
) -> Result<Json<SavedPinView>, ApiError> {
    let id = required_id(&params)?;
    require_investigation(&state, id).await?;
    let pin = pin_member(
        &state.db,
        id,
        &target_from_view(&body.target),
        origin_from_view(body.origin),
        body.note.as_deref(),
    )
    .await
    .map_err(|e| bad_request(format!("pin failed: {e}")))?;
    Ok(Json(pin_view(&pin)))
}

/// `POST /api/investigation/unpin?id=…`.
///
/// `POST` rather than `DELETE` because the target is a structured body, not an id in a path — an
/// edge pin is three strings and a direction. The response says whether anything was removed.
pub async fn unpin_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
    Json(body): Json<UnpinRequest>,
) -> Result<Json<UnpinResultView>, ApiError> {
    let id = required_id(&params)?;
    require_investigation(&state, id).await?;
    let removed = unpin_member(&state.db, id, &target_from_view(&body.target))
        .await
        .map_err(|e| bad_request(format!("unpin failed: {e}")))?;
    Ok(Json(UnpinResultView { removed }))
}

/// `PATCH /api/investigation/pin?id=…` — replace one member's note.
///
/// A note on a member that is not pinned is a `404`: `set_pin_note` errors rather than quietly
/// succeeding, and this handler keeps that distinction rather than flattening it into a 200.
pub async fn pin_note_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
    Json(body): Json<PinNoteRequest>,
) -> Result<Json<SavedPinView>, ApiError> {
    let id = required_id(&params)?;
    require_investigation(&state, id).await?;
    let pin = set_pin_note(&state.db, id, &target_from_view(&body.target), &body.note)
        .await
        .map_err(|e| {
            not_found(format!(
                "the note was not saved — that member is not pinned to {id}: {e}"
            ))
        })?;
    Ok(Json(pin_view(&pin)))
}

// ─── CNV-03: saving the visible subgraph ───────────────────────────────────────────────────────

/// Refuse rather than truncate, and say so in the words the analyst needs.
///
/// A save above the cap is **not** cut down to fit. Truncating it would be this requirement's own
/// defect one level down — "what was saved is exactly what was visible" — and it would be
/// invisible: an investigation of 500 entities looks identical whether 500 was the whole set or
/// the first 500 of four thousand.
pub fn save_refused_copy(visible: usize, cap: u32) -> String {
    format!(
        "{visible} nodes are visible, which is more than the {cap} a single save may write. \
         Narrow the filters or collapse a cluster and save again — this save was refused rather \
         than cut down to fit, because a saved subgraph that quietly dropped the rest would look \
         exactly like a complete one."
    )
}

/// What the server saw when it recomputed, against what the canvas said it was showing.
pub fn save_mismatch_copy(expected: usize, recomputed: usize) -> String {
    format!(
        "Nothing was saved. The canvas reported {expected} visible nodes and re-running the same \
         filters on the server produced {recomputed}. A saved investigation must be exactly what \
         was on screen, so the disagreement is an error rather than a set to pick from."
    )
}

/// The sentence a subgraph save writes into the investigation's notes.
///
/// Two facts, and the second is the one that would otherwise be lost: **what the canvas had
/// already cut before the filters ran**. 45-13's neighbourhood node cap bites before this save
/// sees anything, so a set saved out of a capped expansion is a window on a window. The save
/// cannot un-cut it; what it can do is refuse to be the place the fact disappears.
pub fn subgraph_provenance_note(
    entity_count: usize,
    edge_count: usize,
    saved_at: &str,
    cut: Option<&urban_integrity_workbench_dto::investigation::ExpansionCutView>,
) -> String {
    let head = format!(
        "Saved {entity_count} entities and {edge_count} edges from the canvas on {saved_at}."
    );
    let Some(cut) = cut else {
        return format!("{head} No neighbourhood expansion was running: the filters ran over the whole graph population.");
    };
    if cut.truncated {
        format!(
            "{head} The neighbourhood of {} within {} hops was cut at the node cap {} BEFORE \
             these filters ran: {} of {} nodes were never on screen and are not in this set.",
            cut.seed_id,
            cut.hops,
            cut.node_cap,
            cut.total_discovered.saturating_sub(cut.shown),
            cut.total_discovered,
        )
    } else {
        format!(
            "{head} The canvas was showing every one of the {} nodes within {} hops of {} (node \
             cap {}), so nothing was cut before these filters ran.",
            cut.total_discovered, cut.hops, cut.seed_id, cut.node_cap,
        )
    }
}

/// `POST /api/investigation/subgraph?id=…` — CNV-03's "save the visible subgraph".
///
/// # What this handler will not do
///
/// It will not save the result of its own query. The browser sends the ids it has on screen and
/// the filter request that produced them; this handler re-runs that request against the snapshot
/// and writes pins **only if the two sets are equal**. A save that quietly re-ran an unfiltered
/// query is the defect CNV-03 is written against, and it is invisible after the fact — the saved
/// investigation looks plausible, just bigger than what the analyst was looking at.
///
/// # What it will not do either
///
/// Truncate. Above `SAVE_SUBGRAPH_NODE_CAP` the save is refused with [`save_refused_copy`].
pub async fn save_subgraph_handler(
    Extension(handle): Extension<
        Arc<urban_integrity_core::graph::coordinator::GraphCoordinatorHandle>,
    >,
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<InvestigationQuery>,
    Json(body): Json<urban_integrity_workbench_dto::investigation::SaveSubgraphRequest>,
) -> Result<Json<urban_integrity_workbench_dto::investigation::SaveSubgraphResultView>, ApiError> {
    use std::collections::BTreeSet;
    use urban_integrity_core::persistence::queries::{query_alerts, AlertFilter};
    use urban_integrity_workbench_dto::caps::SAVE_SUBGRAPH_NODE_CAP;

    let id = required_id(&params)?;
    require_investigation(&state, id).await?;

    let Some(scope) = crate::boot::resolve_population_scope(&state, &handle) else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "population not yet available: the graph snapshot is still baking — retry shortly"
            })),
        ));
    };
    let scope = scope.clone();

    let alerts = query_alerts(&state.db, &AlertFilter::default())
        .await
        .map_err(|e| internal(format!("alert scores unavailable: {e}")))?;
    let scores = crate::graph::canvas_filter::scores_by_entity(&alerts.alerts);

    let storage = handle.storage.clone();
    let snapshot = Arc::clone(&handle.snapshot.load());
    let filter = body.filter.clone();
    let (node_ids, edges) = tokio::task::spawn_blocking(move || {
        let candidates = crate::graph::canvas_filter::gather(
            &storage,
            &snapshot,
            &scope,
            &filter.scope_ids,
            &scores,
        );
        crate::graph::canvas_filter::visible_subgraph(&candidates.nodes, &candidates.edges, &filter)
    })
    .await
    .map_err(|e| internal(format!("visible subgraph could not be resolved: {e}")))?;

    // Compared as sets: the canvas orders by its own focus set and the server by candidate order,
    // and an ordering difference is not a disagreement about what was on screen.
    let recomputed: BTreeSet<&str> = node_ids.iter().map(String::as_str).collect();
    let expected: BTreeSet<&str> = body.expected_node_ids.iter().map(String::as_str).collect();
    if recomputed != expected {
        return Err((
            axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": save_mismatch_copy(expected.len(), recomputed.len())
            })),
        ));
    }

    if node_ids.len() > SAVE_SUBGRAPH_NODE_CAP as usize {
        return Err(bad_request(save_refused_copy(
            node_ids.len(),
            SAVE_SUBGRAPH_NODE_CAP,
        )));
    }

    for entity_id in &node_ids {
        pin_member(
            &state.db,
            id,
            &PinTarget::Entity {
                entity_id: entity_id.clone(),
            },
            PinOrigin::Subgraph,
            // `None` PRESERVES an existing note (45-07). A bulk save must never erase what the
            // analyst typed on a member they had already pinned by hand from a profile.
            None,
        )
        .await
        .map_err(|e| internal(format!("subgraph save failed on {entity_id}: {e}")))?;
    }
    for edge in &edges {
        pin_member(
            &state.db,
            id,
            &PinTarget::Edge {
                from: edge.from.clone(),
                to: edge.to.clone(),
                relation: edge.relation.clone(),
            },
            PinOrigin::Subgraph,
            None,
        )
        .await
        .map_err(|e| {
            internal(format!(
                "subgraph save failed on edge {} -> {}: {e}",
                edge.from, edge.to
            ))
        })?;
    }

    let note = subgraph_provenance_note(
        node_ids.len(),
        edges.len(),
        &chrono::Utc::now().to_rfc3339(),
        body.expansion_cut.as_ref(),
    );
    // The set-level fact has no column of its own, and the alternative is dropping it. Appending
    // to the investigation's OWN notes is a read-modify-write of one field the analyst owns — not
    // of anybody's member notes, which is the read-modify-write 45-07 designed two tables to
    // avoid. Its weakness is stated in this plan's summary: an analyst can edit the line away.
    let current = require_investigation(&state, id).await?;
    let merged = if current.notes.trim().is_empty() {
        note.clone()
    } else {
        format!("{}\n\n{note}", current.notes.trim_end())
    };
    set_investigation_notes(&state.db, id, &merged)
        .await
        .map_err(|e| internal(format!("the save provenance note was not written: {e}")))?;

    Ok(Json(
        urban_integrity_workbench_dto::investigation::SaveSubgraphResultView {
            entity_count: node_ids.len() as u64,
            edge_count: edges.len() as u64,
            provenance_note: note,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire vocabulary and the storage vocabulary are the same one, in both directions.
    ///
    /// PROVE THIS CAN FAIL: swapping the `Canvas` and `Subgraph` arms of `origin_from_view` makes
    /// the origin round trip fail. That mutation was run.
    #[test]
    fn every_target_and_origin_round_trips_through_both_conversions() {
        let targets = [
            PinTargetView::Entity {
                entity_id: "GB-COH:X06999001".to_string(),
            },
            PinTargetView::Edge {
                from: "GB-COH:X06999001".to_string(),
                to: "contract:X9999-c1".to_string(),
                relation: "awarded_to".to_string(),
            },
        ];
        for target in &targets {
            assert_eq!(&target_view(&target_from_view(target)), target);
        }

        // Exhaustive over the four origins: a fifth added to either enum without being added
        // here would NOT fail to compile on its own (this is an array literal, not a match) — the
        // exhaustiveness that actually catches a missing variant lives in `origin_view`/
        // `origin_from_view`'s own match arms, which fail `cargo build` on an uncovered variant.
        // This loop's job is only to prove every variant round-trips once both conversions exist.
        for origin in [
            PinOriginView::Profile,
            PinOriginView::Canvas,
            PinOriginView::Subgraph,
            PinOriginView::Alert,
        ] {
            assert_eq!(origin_view(origin_from_view(origin)), origin);
        }
    }

    /// A save records what the canvas had **already** cut, and says plainly when nothing was.
    ///
    /// The three arms are three different facts and must not collapse into one sentence: no
    /// expansion was running; an expansion was running and fitted; an expansion was running and
    /// was cut at 45-13's node cap before these filters saw anything.
    ///
    /// PROVE THIS CAN FAIL: drop the `cut.truncated` branch (always use the "nothing was cut"
    /// wording) and the third assertion fails, which is the one that matters — a saved subgraph
    /// that silently dropped the cap is this requirement's defect one level down.
    #[test]
    fn a_save_records_what_the_canvas_had_already_cut() {
        use urban_integrity_workbench_dto::investigation::ExpansionCutView;

        let none = subgraph_provenance_note(41, 57, "2026-01-01T00:00:00Z", None);
        assert!(none.contains("Saved 41 entities and 57 edges"));
        assert!(none.contains("No neighbourhood expansion was running"));

        let fitted = ExpansionCutView {
            seed_id: "GB-COH:X6999001".to_string(),
            hops: 2,
            node_cap: 250,
            total_discovered: 40,
            shown: 40,
            truncated: false,
        };
        let whole = subgraph_provenance_note(41, 57, "2026-01-01T00:00:00Z", Some(&fitted));
        assert!(whole.contains("every one of the 40 nodes within 2 hops"));
        assert!(whole.contains("nothing was cut"));

        let cut = ExpansionCutView {
            total_discovered: 1_842,
            shown: 250,
            truncated: true,
            ..fitted
        };
        let windowed = subgraph_provenance_note(250, 600, "2026-01-01T00:00:00Z", Some(&cut));
        assert!(
            windowed.contains("1592 of 1842 nodes were never on screen"),
            "the note must name the cut, not the shown: {windowed}"
        );
        assert!(windowed.contains("BEFORE"));
        // The three arms really are three sentences, not one with a variable in it.
        assert_ne!(none, whole);
        assert_ne!(whole, windowed);
    }

    /// A save above the cap is refused in words that say it was refused **rather than cut down**,
    /// and a mismatch names both counts.
    ///
    /// PROVE THIS CAN FAIL: reword `save_refused_copy` to promise a partial save and the
    /// "refused rather" assertion fails.
    #[test]
    fn the_refusal_says_it_refused_and_the_mismatch_names_both_counts() {
        let refused = save_refused_copy(4_000, 500);
        assert!(refused.contains("4000 nodes are visible"));
        assert!(refused.contains("maximum") || refused.contains("500"));
        assert!(
            refused.contains("refused rather"),
            "the copy must say it refused rather than truncated: {refused}"
        );

        let mismatch = save_mismatch_copy(41, 1_842);
        assert!(mismatch.starts_with("Nothing was saved."));
        assert!(mismatch.contains("41") && mismatch.contains("1842"));
    }

    /// The edge pin's direction survives the crossing. `A -> B` and `B -> A` are two facts, and
    /// the storage layer's `member_key` keys them separately — a conversion that lost the
    /// direction would silently collapse two pins into one.
    #[test]
    fn an_edge_pins_direction_is_not_lost_in_translation() {
        let forward = target_from_view(&PinTargetView::Edge {
            from: "a".to_string(),
            to: "b".to_string(),
            relation: "directs".to_string(),
        });
        let backward = target_from_view(&PinTargetView::Edge {
            from: "b".to_string(),
            to: "a".to_string(),
            relation: "directs".to_string(),
        });
        assert_ne!(forward.member_key(), backward.member_key());
    }
}
