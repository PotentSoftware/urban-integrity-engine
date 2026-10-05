//! `GET /api/entity/profile`, `GET /api/entity/disambiguate`, `GET /api/entity/neighbourhood` —
//! the entity half of the workbench HTTP seam (PIV-01, PIV-02, PIV-04, PIV-06, CNV-01).
//!
//! # PIV-06 / D-01 in one sentence
//!
//! **Every fact these handlers return was read by `crates/core::persistence`.** There is no
//! SurrealQL in this crate, no second profile assembler, and nothing here infers a fact the read
//! path did not state. D-01's reason is the one that matters: the CLI `entity` command, the
//! GraphQL resolver and this screen all render `EntityProfile`, so a second assembler would let
//! them disagree about one company and nothing would fail.
//!
//! The functions behind each route, named so a reader can check the claim without tracing calls:
//!
//! | Route | `crates/core::persistence` |
//! |---|---|
//! | `GET /api/entity/profile` | `queries::query_entity_by_id_as_at` (+ the pure `queries::findings_naming_entity`) |
//! | `GET /api/entity/disambiguate` | `queries::{query_entity_by_id, query_entity_by_ch_number, query_entity_by_name}` |
//! | `GET /api/entity/neighbourhood` | `queries::query_entity_neighbourhood` |
//!
//! Two in-memory reads are deliberately *not* persistence calls and are not facts: the boot-time
//! `LabelIndex` supplies display **names** for ids (NAV-06's own name source, already what
//! `GET /api/entities/search` returns), and `urban_integrity_probe::battery_evidence_coverage`
//! counts the probe battery, which is a property of the code rather than of the analysis.
//!
//! # No client parameter ever selects a database
//!
//! T-43-05-04's discipline, restated: `state.db` is the only connection these handlers touch. The
//! client-supplied values (`id`, `q`, `as_at`, `section`, `cursor`, `page_size`, `hops`,
//! `node_cap`) all select *within* the already-fixed analysis; none of them can redirect a request
//! to another one.

pub mod neighbourhood;
pub mod profile_map;

use std::sync::Arc;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use serde_json::json;

use urban_integrity_core::persistence::queries::{
    findings_naming_entity, is_slug_shape, is_valid_ch_number, query_entity_by_ch_number,
    query_entity_by_id, query_entity_by_id_as_at, query_entity_by_name, EntityProfile,
};
use urban_integrity_probe::battery_evidence_coverage;
use urban_integrity_probe::run::build_battery;
use urban_integrity_schema::types::verdict::Finding;
use urban_integrity_workbench_dto::{
    DisambiguationRowView, DisambiguationView, EntityProfileDto, EntityStatusView,
};

use crate::boot::WorkbenchState;
use crate::entity::profile_map::{entity_ref_for, to_dto, window_from_request};

/// The shape every error body on this surface takes — the same `{"error": …}` object
/// `analysis.rs` and `graph/handlers.rs` already return, so the browser has one error shape to
/// read across the whole API.
pub(crate) type ApiError = (StatusCode, Json<serde_json::Value>);

pub(crate) fn bad_request(message: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.to_string() })),
    )
}

pub(crate) fn not_found(message: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": message.to_string() })),
    )
}

pub(crate) fn internal(message: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message.to_string() })),
    )
}

/// Query parameters for [`profile_handler`].
///
/// `section`/`cursor` together are one **Show more** press: `cursor` is the opaque token the
/// previous response's `next_cursor` carried (see `profile_map`'s doc comment). `as_at` is an
/// ISO-8601 date; PIV-04's filter **marks** rows and removes none, so setting it never shortens a
/// section — it changes each row's `temporal.status`.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ProfileQuery {
    id: Option<String>,
    as_at: Option<String>,
    section: Option<String>,
    cursor: Option<String>,
    page_size: Option<u32>,
}

/// `GET /api/entity/profile?id=…` — the whole profile screen's data (PIV-02).
///
/// 404 carries the phase 45 UI spec's own sentence, so the page renders the copy the spec fixes rather
/// than inventing one at the boundary.
pub async fn profile_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<ProfileQuery>,
) -> Result<Json<EntityProfileDto>, ApiError> {
    let id = params
        .id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_request("`id` is required"))?;

    let as_at =
        match params
            .as_at
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => None,
            Some(raw) => Some(chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(
                |_| bad_request(format!("`as_at` must be an ISO-8601 date, got {raw:?}")),
            )?),
        };

    let window = window_from_request(
        params.section.as_deref(),
        params.cursor.as_deref(),
        params.page_size,
    )
    .map_err(bad_request)?;

    let profile = query_entity_by_id_as_at(&state.db, id, as_at)
        .await
        .map_err(|e| internal(format!("entity profile read failed: {e}")))?
        .ok_or_else(|| not_found(format!("No entity with id {id} exists in this analysis.")))?;

    // The probe battery is constructed, never run: `battery_evidence_coverage` reads each probe's
    // `evidence_scope()` declaration, which is a property of the code. This is what makes the
    // findings section's "{k} of {m} probes report only aggregate counts" figure available even
    // when no run has happened in this process (45-05).
    let coverage = battery_evidence_coverage(&build_battery(
        state.catalog.as_ref().clone(),
        state.db_name.clone(),
        state.oc_gates,
    ));

    // Everything below is synchronous, which is what makes holding this std::sync lock safe: no
    // `.await` is reached while the guard is alive.
    let cached = state
        .last_findings
        .read()
        .expect("last_findings lock poisoned");
    let (entries, run_at) = match cached.as_ref() {
        Some(c) => (c.entries.as_slice(), Some(c.run_at)),
        None => (&[][..], None),
    };
    let findings: Vec<Finding> = entries.iter().map(|e| e.finding.clone()).collect();
    let run_observed_at = run_at.map(|t| t.to_rfc3339());

    // `profile.alerts` is the same population `alerts_naming_entity` returns — the same scan, the
    // same withdrawn-alert filter, the same sort (`assemble_entity_profile` step 4). Passing it
    // here rather than calling `query_findings_naming_entity` avoids a second full scan of
    // `graph_events` per profile load without changing the answer.
    let index = findings_naming_entity(
        id,
        profile.ch_number.as_deref(),
        &findings,
        &profile.alerts,
        &coverage,
        run_observed_at.as_deref(),
    );

    let dto = to_dto(&profile, &state.label_index, &index, entries, &window).map_err(internal)?;
    drop(cached);
    Ok(Json(dto))
}

/// Query parameters for [`disambiguate_handler`].
#[derive(Debug, Default, serde::Deserialize)]
pub struct DisambiguateQuery {
    q: Option<String>,
}

/// `resolve_entity`'s own `entity_ids.truncate(10)` and `query_entity_by_name`'s "returns at most
/// 10 profiles (D-02 hard cap)" are the same number. It is reproduced here **only** to detect that
/// the cap bit, never to apply one — the read path applies it and this handler reports it.
const MATCH_CAP: usize = 10;

/// `GET /api/entity/disambiguate?q=…` — PIV-01's list, which opens *before* any profile does.
///
/// The resolution order mirrors `crates/cli/src/commands/entity.rs` exactly — slug-shaped input to
/// `query_entity_by_id`, a valid Companies House number to `query_entity_by_ch_number`, anything
/// else (and a slug that misses) to `query_entity_by_name` — so the workbench's list and the CLI's
/// cannot disagree about which entities a query matches.
///
/// An empty `q`, or a `q` that matches nothing, is `200` with an empty `rows`: "No entity matches"
/// is an empty list, not a failure state (NAV-06's inherited rule).
pub async fn disambiguate_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<DisambiguateQuery>,
) -> Result<Json<DisambiguationView>, ApiError> {
    let q = params.q.as_deref().map(str::trim).unwrap_or("");
    if q.is_empty() {
        return Ok(Json(DisambiguationView {
            query: String::new(),
            rows: Vec::new(),
            more_may_exist: false,
        }));
    }

    let mut profiles: Vec<EntityProfile> = Vec::new();

    if is_slug_shape(q) {
        if let Some(p) = query_entity_by_id(&state.db, q)
            .await
            .map_err(|e| internal(format!("entity lookup failed: {e}")))?
        {
            profiles.push(p);
        }
    }
    if profiles.is_empty() && is_valid_ch_number(q) {
        if let Some(p) = query_entity_by_ch_number(&state.db, q)
            .await
            .map_err(|e| internal(format!("entity lookup failed: {e}")))?
        {
            profiles.push(p);
        }
    }
    if profiles.is_empty() {
        profiles = query_entity_by_name(&state.db, q)
            .await
            .map_err(|e| internal(format!("entity search failed: {e}")))?;
    }

    let more_may_exist = profiles.len() >= MATCH_CAP;
    let rows = profiles
        .iter()
        .map(|p| DisambiguationRowView {
            entity: entity_ref_for(&state.label_index, &p.id),
            status: match &p.status {
                urban_integrity_schema::types::node::EntityStatus::Active => {
                    EntityStatusView::Active
                }
                urban_integrity_schema::types::node::EntityStatus::Dissolved(on) => {
                    EntityStatusView::Dissolved {
                        on: on.format("%Y-%m-%d").to_string(),
                    }
                }
                urban_integrity_schema::types::node::EntityStatus::InLiquidation(on) => {
                    EntityStatusView::InLiquidation {
                        on: on.format("%Y-%m-%d").to_string(),
                    }
                }
            },
            ward: ward_of(&state.label_index, p),
        })
        .collect();

    Ok(Json(DisambiguationView {
        query: q.to_string(),
        rows,
        more_may_exist,
    }))
}

/// The ward a candidate's recorded work sits in, or `None` when the read path records none.
///
/// **Derived from the profile's own rows, never from a second query.** `Represents` runs
/// Ward → *Contract* in this graph (`ingestor/planning/mapper.rs`: "1 Represents edge (Ward →
/// Contract)"), not Ward → Entity, so a company has no ward of its own — it has the wards of the
/// contracts and planning applications awarded to it. The most frequent `ward_code` across both
/// sections is that answer, with ties broken by the code ascending so the same profile always
/// yields the same ward.
///
/// `None` is common and correct: Contracts Finder awards carry no `ward_code` at all
/// (`query_entity_lpa_presence`'s own doc comment says so), so a CF-only supplier has no ward
/// here. A renderer must treat the missing segment as absent rather than as an error — this is
/// the majority case for companies, and the minority for planning applicants.
fn ward_of(
    label_index: &crate::graph::label_index::LabelIndex,
    profile: &EntityProfile,
) -> Option<urban_integrity_workbench_dto::EntityRef> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for code in profile
        .contracts
        .iter()
        .filter_map(|c| c.ward_code.as_deref())
        .chain(
            profile
                .planning_applications
                .iter()
                .filter_map(|p| p.ward_code.as_deref()),
        )
    {
        *counts.entry(code).or_insert(0) += 1;
    }
    // `BTreeMap` iterates by code ascending, and `max_by_key` keeps the LAST maximum, so the
    // comparison is inverted to keep the first — the lexicographically smallest code among ties.
    let winner = counts
        .into_iter()
        .max_by_key(|(code, count)| (*count, std::cmp::Reverse(*code)))?
        .0;
    Some(entity_ref_for(label_index, &format!("ward:{winner}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::persistence::queries::{ContractRow, PlanningApplicationRow};
    use urban_integrity_workbench_dto::temporal::{AsAtStatus, EffectiveRange, Temporal};
    use urban_integrity_workbench_dto::Provenance;

    fn unattributed() -> Provenance {
        Provenance::Unattributed {
            reason: "test fixture".to_string(),
        }
    }

    fn temporal() -> Temporal {
        Temporal {
            range: EffectiveRange::Unrecorded {
                basis: "test fixture".to_string(),
            },
            status: AsAtStatus::NotFiltered,
        }
    }

    fn profile_with_wards(contracts: &[&str], planning: &[&str]) -> EntityProfile {
        let mut p = empty_profile();
        p.contracts = contracts
            .iter()
            .map(|code| ContractRow {
                id: format!("CF:fixture-{code}"),
                name: "c".to_string(),
                value_gbp: None,
                status: None,
                published: None,
                ward_code: Some((*code).to_string()),
                tender_ref: None,
                application_ref: None,
                provenance: unattributed(),
                temporal: temporal(),
            })
            .collect();
        p.planning_applications = planning
            .iter()
            .map(|code| PlanningApplicationRow {
                id: format!("planning:fixture-{code}"),
                name: "p".to_string(),
                status: None,
                published: None,
                ward_code: Some((*code).to_string()),
                application_ref: None,
                provenance: unattributed(),
                temporal: temporal(),
            })
            .collect();
        p
    }

    fn empty_profile() -> EntityProfile {
        EntityProfile {
            id: "GB-COH:X06999001".to_string(),
            name: "Example Holdings Ltd".to_string(),
            ch_number: Some("X06999001".to_string()),
            status: urban_integrity_schema::types::node::EntityStatus::Active,
            incorporation_date: None,
            identity_provenance: unattributed(),
            addresses: vec![],
            directors: vec![],
            contracts: vec![],
            planning_applications: vec![],
            alerts: vec![],
            related_entities: vec![],
            lpa_presence: vec![],
            beneficial_owners: vec![],
            disqualifications: vec![],
            offshore_links: vec![],
            trustee_of: vec![],
            owns_property: vec![],
            donates_to: vec![],
            osint_links: vec![],
            sanctions_status: None,
            osint_alerts: vec![],
            sub_candidate_links: vec![],
            as_at: None,
        }
    }

    /// The ward is the commonest recorded code across both award sections, not the first one
    /// seen — a planning applicant with nine applications in one ward and one in another is in
    /// the first.
    #[test]
    fn the_ward_is_the_commonest_recorded_code() {
        let index = crate::graph::label_index::LabelIndex::default();
        let profile = profile_with_wards(&["X05999002"], &["X05999001", "X05999001"]);
        let ward = ward_of(&index, &profile).expect("a ward");
        assert_eq!(ward.id, "ward:X05999001");
    }

    /// Ties break on the code ascending, so one profile always yields one ward.
    #[test]
    fn a_tie_breaks_deterministically_on_the_code() {
        let index = crate::graph::label_index::LabelIndex::default();
        let a = profile_with_wards(&["X05999002"], &["X05999001"]);
        let b = profile_with_wards(&["X05999001"], &["X05999002"]);
        assert_eq!(
            ward_of(&index, &a).map(|w| w.id),
            ward_of(&index, &b).map(|w| w.id)
        );
        assert_eq!(ward_of(&index, &a).expect("a ward").id, "ward:X05999001");
    }

    /// No recorded ward code is `None`, not a placeholder ward — the majority case for a
    /// Contracts Finder supplier, whose awards carry no ward code at all.
    #[test]
    fn an_entity_with_no_ward_code_has_no_ward() {
        let index = crate::graph::label_index::LabelIndex::default();
        assert!(ward_of(&index, &empty_profile()).is_none());
    }
}
