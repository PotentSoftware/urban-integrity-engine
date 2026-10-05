//! The one `graph_events` writer for fixtures and seeders. Fail-loud on every write: a `CREATE`
//! that returns a statement error is an `Err`, never a silent no-op (the PPD/ICIJ lessons).
//! Event ids are caller-supplied so a rerun of the same fixture produces the same rows.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_schema::types::edge::EdgeWeight;
use urban_integrity_schema::types::node::NodeWeight;

/// Fixed event timestamp for every seeded row, so a reseed is byte-identical.
///
/// `graph_events.timestamp` is an integer millisecond field, not a `datetime`, so this never
/// meets SurrealDB 3.x's string-to-datetime coercion rule.
pub const FIXTURE_TIMESTAMP_MS: u64 = 1_700_000_000_000;

pub async fn seed_node_event(
    db: &Surreal<Any>,
    event_id: &str,
    entity_id: &str,
    weight: &NodeWeight,
) -> anyhow::Result<()> {
    seed_node_event_with_kind(db, event_id, "NodeAdded", entity_id, weight).await
}

/// As [`seed_node_event`], with the `graph_events.kind` taken as a parameter instead of the
/// literal `"NodeAdded"` — the one write path both a fresh node and a status-flip `NodeUpdated`
/// row (Phase 49's `B` liquidation flip) go through.
pub async fn seed_node_event_with_kind(
    db: &Surreal<Any>,
    event_id: &str,
    kind: &str,
    entity_id: &str,
    weight: &NodeWeight,
) -> anyhow::Result<()> {
    let weight_json = serde_json::to_string(weight)?;
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": FIXTURE_TIMESTAMP_MS,
                "kind": kind,
                "payload": { "entity_id": entity_id, "weight_json": weight_json },
            }),
        ))
        .await?
        .check()?;
    Ok(())
}

pub async fn seed_edge_event(
    db: &Surreal<Any>,
    event_id: &str,
    src_id: &str,
    dst_id: &str,
    weight: &EdgeWeight,
) -> anyhow::Result<()> {
    let weight_json = serde_json::to_string(weight)?;
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": FIXTURE_TIMESTAMP_MS,
                "kind": "EdgeAdded",
                "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
            }),
        ))
        .await?
        .check()?;
    Ok(())
}

/// Seed one planted `identity_links` row (Phase 49, EVM-04/EVM-05).
///
/// Modelled on `crates/probe/src/psc_beneficial_ownership.rs`'s local `seed_identity_link` test
/// helper — the all-`SET` form with `resolved_at` bound as a `chrono::DateTime<Utc>`, per
/// the SurrealDB 3.x datetime-coercion rule (a bare `CONTENT` with a string date drops
/// the row silently). `resolved_at` is fixed to the epoch plus [`FIXTURE_TIMESTAMP_MS`] rather
/// than `chrono::Utc::now()` so a reseed is byte-identical.
pub async fn seed_identity_link(
    db: &Surreal<Any>,
    link: &crate::planted::PlantedIdentityLink,
) -> anyhow::Result<()> {
    let resolved_at = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
        + chrono::Duration::milliseconds(FIXTURE_TIMESTAMP_MS as i64);
    db.query(
        "CREATE type::record('identity_links', $key) SET \
             source_node_id = $sni, target_node_id = $tni, resolution_type = 'exact_company_number', \
             confidence = $conf, source = $source, resolved_at = $resolved_at, \
             evidence_json = '{}', status = $status",
    )
    .bind(("key", link.key.to_string()))
    .bind(("sni", link.source_node_id.to_string()))
    .bind(("tni", link.target_node_id.to_string()))
    .bind(("conf", link.confidence))
    .bind(("source", link.source.to_string()))
    .bind(("status", link.status.to_string()))
    .bind(("resolved_at", resolved_at))
    .await?
    .check()?;
    Ok(())
}

/// Seed one `enrichment_runs` marker for `source` (Phase 49, EVM-04/EVM-05).
///
/// Modelled on the identical helper duplicated in `property_nodes.rs`/`identity_link_registry.rs`
/// per this codebase's per-module-duplicate-seeder convention. `last_run_at` is UNIX **seconds**,
/// not milliseconds — `psc_beneficial_ownership.rs`'s `day_of` doc comment is the citation for
/// that unit. This marker only needs to prove "this adapter ran", so the four count fields are
/// `0`.
pub async fn seed_enrichment_run(db: &Surreal<Any>, source: &str) -> anyhow::Result<()> {
    db.query(format!(
        "UPSERT enrichment_runs:{source} SET source = '{source}', last_run_at = $last_run_at, \
         candidates_found = $candidates_found, auto_linked = $auto_linked, \
         pending_review = $pending_review, discarded = $discarded"
    ))
    .bind(("last_run_at", (FIXTURE_TIMESTAMP_MS / 1000) as i64))
    .bind(("candidates_found", 0i64))
    .bind(("auto_linked", 0i64))
    .bind(("pending_review", 0i64))
    .bind(("discarded", 0i64))
    .await?
    .check()?;
    Ok(())
}

/// Seed one planted `MotifDetected` `graph_events` row (Phase 49, EVM-04/EVM-05).
///
/// `result_json` is a JSON **string**, matching every real writer's `payload: { "result_json":
/// "<json string>" }` shape exactly — never a nested object (that is the coercion trap
/// `crates/core/src/persistence/surreal.rs`'s `MotifDetected` branch guards against).
pub async fn seed_motif_event(
    db: &Surreal<Any>,
    motif: &crate::planted::PlantedMotifEvent,
) -> anyhow::Result<()> {
    let result_json = serde_json::json!({
        "motif_type": motif.motif_type,
        "participants": motif.participants,
        "risk_score": motif.risk_score,
        "unique_hash": 0u64,
        "evidence_snapshot": "{}",
        "detected_at_event": 0u64,
        "version": 1u32,
        "last_seen_at": "2026-01-01T00:00:00Z",
        "score_breakdown": {
            "s_phoenix": 0.0,
            "s_proximity": 0.0,
            "s_clustering": 0.0,
            "p_base": 0.0,
            "m_env": 1.0,
            "p_final": motif.risk_score,
            "imd_rank": null,
            "ward_code": null,
        },
    })
    .to_string();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": motif.event_id,
                "timestamp": FIXTURE_TIMESTAMP_MS,
                "kind": "MotifDetected",
                "payload": { "result_json": result_json },
            }),
        ))
        .await?
        .check()?;
    Ok(())
}

/// Seed the planted `land_flip_runs:current` marker plus its `land_flip_gate1_survivors` rows
/// (Phase 50 plan 10).
///
/// Mirrors `core::persistence::land_flip_runs::record_land_flip_run`'s exact UPSERT SQL shape —
/// `test-support` cannot depend on `core` (dependency direction), so this is a deliberate
/// duplicate, not a shared import (the same posture `land_flip_capability.rs`'s own module doc
/// comment documents for its predicate). Every field this plan's `PlantedLandFlipRun` does not
/// name is `0`. `finished_at_ms` is bound as an `i64` (the field is `land_flip_runs SCHEMALESS`
/// with an integer-millisecond field, never a `datetime`), so this never meets SurrealDB 3.x's
/// string-to-datetime coercion trap.
pub async fn seed_land_flip_run(
    db: &Surreal<Any>,
    run: &crate::planted::PlantedLandFlipRun,
) -> anyhow::Result<()> {
    db.query(
        "UPSERT land_flip_runs:current SET \
         applications_scanned = $applications_scanned, \
         candidates_written = 0, \
         candidate_writes_failed = 0, \
         postcode_only_matches = 0, \
         gate1_no_ppd_match = $gate1_no_ppd_match, \
         gate3_query_failed = 0, \
         gate3_no_corroboration = 0, \
         gate2_no_anchor_date = 0, \
         gate2_outside_window = 0, \
         candidate_apps = $candidate_apps, \
         finished_at_ms = $finished_at_ms",
    )
    .bind(("applications_scanned", run.applications_scanned))
    .bind(("gate1_no_ppd_match", run.gate1_no_ppd_match))
    .bind(("candidate_apps", run.candidate_apps))
    .bind(("finished_at_ms", FIXTURE_TIMESTAMP_MS as i64))
    .await?
    .check()?;
    for s in &run.survivors {
        db.query("CREATE land_flip_gate1_survivors CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "planning_ref": s.planning_ref,
                    "applicant_id": s.applicant_id,
                    "location_id": s.location_id,
                }),
            ))
            .await?
            .check()?;
    }
    Ok(())
}

/// Write the whole planted fixture. Event ids derive from entity ids, so the same fixture always
/// produces the same `event_id` set.
pub async fn seed_planted(
    db: &Surreal<Any>,
    graph: &crate::planted::PlantedGraph,
) -> anyhow::Result<(usize, usize)> {
    for (id, weight) in &graph.nodes {
        seed_node_event(db, &format!("NodeAdded-{}", id.0), &id.0, weight).await?;
    }
    for (i, e) in graph.edges.iter().enumerate() {
        seed_edge_event(
            db,
            &format!("EdgeAdded-{i}-{}-{}", e.src.0, e.dst.0),
            &e.src.0,
            &e.dst.0,
            &e.weight,
        )
        .await?;
    }
    for link in &graph.identity_links {
        seed_identity_link(db, link).await?;
    }
    for source in &graph.enrichment_run_sources {
        seed_enrichment_run(db, source).await?;
    }
    for motif in &graph.motif_events {
        seed_motif_event(db, motif).await?;
    }
    seed_land_flip_run(db, &graph.land_flip_run).await?;
    // `entity_in_liquidation`'s second argument becomes the node's whole new `name` — passing
    // anything other than B's own name ("Planted Beta Ltd", `planted.rs`'s `topology()`) silently
    // renames B mid-fixture instead of only flipping its status. Found by 49-11's full
    // `URBAN_DB_TESTS=1 cargo test --workspace` gate: a stray "Planted Cluster Buyer" placeholder
    // here (49-02) broke two pre-existing `planted_profile.rs` goldens downstream — A's related
    // row for B lost its pivot (the profile renderer could no longer resolve B's post-rename
    // identity), and B's renamed node started spuriously matching the "Planted Cluster" name
    // search (8 -> 9 results). Rule 1 fix: keep the name, flip only the status.
    seed_node_event_with_kind(
        db,
        crate::planted::B_IN_LIQUIDATION_UPDATE_ID,
        "NodeUpdated",
        crate::planted::B,
        &crate::planted::entity_in_liquidation(crate::planted::B, "Planted Beta Ltd").1,
    )
    .await?;
    Ok((graph.nodes.len(), graph.edges.len()))
}
