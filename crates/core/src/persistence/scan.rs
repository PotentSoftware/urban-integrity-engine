//! Paged scans over `graph_events` that are safe against two SurrealDB 3.x defects.
//!
//! Both defects under-report silently — the query succeeds, returns fewer rows than exist, and
//! the caller has no signal that anything was missed. They have already cost this project a CH
//! backfill running against 18% of its input set, and a PSC enrichment running against 0.27% of
//! its candidate companies.
//!
//! 1. **The planner bug.** `kind` is indexed (`idx_graph_events_kind`). Pairing that indexed
//!    equality with a `CONTAINS` predicate on the unindexed `payload.weight_json` makes the
//!    planner take the index path and return a truncated, run-to-run non-deterministic row set,
//!    while `count()` on the identical predicate stays correct. Measured 2026-07-29 against
//!    `an_anglesey_aebaa785`: `count()` reported **14,198** and the identical `SELECT … LIMIT
//!    5000` returned **39**. So `kind` is deliberately kept out of the `WHERE` clause here and
//!    filtered in Rust instead.
//!
//! 2. **Response truncation.** A single large `LIMIT` returns well below the requested count when
//!    `weight_json` is big, so a naive `LIMIT 50000` samples a fraction of the table and reports
//!    success. `START` therefore advances by the number of rows *actually received*, and the loop
//!    ends only on an empty page — a short page means truncation, not the end of the data.
//!
//! This is the `&Surreal<Any>` counterpart of the ingestor's `backfill::scan_events_by_type`,
//! which solves the same problem behind that crate's re-authenticating session wrapper. Both
//! exist because the alternative — every adapter hand-rolling the query — is what produced the
//! 35 sites currently carrying the broken shape.

use serde_json::json;
use surrealdb::{engine::any::Any, Surreal};

/// Page size for full-table event scans. Large enough that the round-trip count stays low, small
/// enough that one page's `weight_json` payload does not dominate the HTTP response.
pub const SCAN_PAGE_SIZE: usize = 20_000;

/// Safety cap on a single scan, so a pathological page sequence cannot loop forever. Well above
/// any real event count for one node/edge type; hitting it is logged as a truncation.
pub(crate) const SCAN_MAX_ROWS: usize = 2_000_000;

/// Every `graph_events` row whose `weight_json` contains `type_marker`, restricted to `kind`.
///
/// Returns the full projection (`id`, `event_id`, `kind`, `payload`) so callers can address rows
/// by primary key afterwards — `WHERE event_id = … AND kind = …` is itself the planner-bug shape.
pub async fn scan_events_by_type(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    scan_events_by_type_paged(db, kind, type_marker, SCAN_PAGE_SIZE).await
}

/// The first `graph_events` row of `kind`/`type_marker` satisfying `pred`, or `None`.
///
/// Same completeness guarantee as [`scan_events_by_type`] — it pages until the row is found or
/// the table is exhausted — but it stops at the first match instead of materialising every row.
///
/// Use this for the "look one entity up by id" paths. Those scan to *find* a row, not to collect
/// rows, and paging the whole table for each lookup makes an O(table) operation out of what the
/// caller experiences as a point read.
pub async fn find_event_by_type<F>(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
    pred: F,
) -> surrealdb::Result<Option<serde_json::Value>>
where
    F: Fn(&serde_json::Value) -> bool,
{
    let mut start = 0usize;
    loop {
        let sql = format!(
            "SELECT id, event_id, kind, payload, timestamp FROM graph_events \
             WHERE payload.weight_json CONTAINS $marker \
             ORDER BY id LIMIT {SCAN_PAGE_SIZE} START {start}"
        );
        let rows: Vec<serde_json::Value> = db
            .query(sql)
            .bind(("marker", json!(type_marker)))
            .await?
            .take(0)?;
        let returned = rows.len();
        if returned == 0 {
            return Ok(None);
        }
        if let Some(hit) = rows
            .into_iter()
            .find(|r| r["kind"].as_str() == Some(kind) && pred(r))
        {
            return Ok(Some(hit));
        }
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                marker = %type_marker,
                "lookup scan hit the safety cap — a match beyond it would be missed"
            );
            return Ok(None);
        }
    }
}

/// Every `graph_events` row of `kind` whose `weight_json` contains **any** of `type_markers`.
///
/// The `OR`-of-markers form (`… CONTAINS 'Officer' OR … CONTAINS 'Entity'`) is just as exposed to
/// the planner bug as the single-marker one, and several adapters use it to pull officers and
/// companies in one pass.
pub async fn scan_events_by_any_type(
    db: &Surreal<Any>,
    kind: &str,
    type_markers: &[&str],
) -> surrealdb::Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for marker in type_markers {
        for row in scan_events_by_type(db, kind, marker).await? {
            // A row can carry more than one marker; key on the record id so the caller never
            // sees it twice.
            let id = row["id"].to_string();
            if seen.insert(id) {
                out.push(row);
            }
        }
    }
    Ok(out)
}

/// Every `graph_events` row whose `weight_json` contains `type_marker`, restricted to **any** of
/// `kinds`.
///
/// The transpose of [`scan_events_by_any_type`]: one marker across several kinds, which is the
/// `kind IN [...] AND … CONTAINS …` form. `kind IN` is exposed to the planner bug for the same
/// reason `kind =` is — the index path is taken and the unindexed `CONTAINS` truncates — and it
/// costs one scan rather than one per kind.
pub async fn scan_events_of_kinds_by_type(
    db: &Surreal<Any>,
    kinds: &[&str],
    type_marker: &str,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    let rows = scan_events_by_type_paged(db, ANY_KIND, type_marker, SCAN_PAGE_SIZE).await?;
    Ok(rows
        .into_iter()
        .filter(|r| r["kind"].as_str().is_some_and(|k| kinds.contains(&k)))
        .collect())
}

/// Sentinel passed to [`scan_events_by_type_paged`] to skip its single-kind filter, leaving the
/// caller to apply its own. Not a real `kind`, so it can never match one.
const ANY_KIND: &str = "\u{0}any-kind";

/// Every `graph_events` row of `kind`, with no predicate on the payload.
///
/// This is the **rule 2** counterpart. A single indexed `kind` equality is not exposed to the
/// planner bug — measured 2026-07-29 against `an_anglesey_aebaa785`, `WHERE kind = 'NodeAdded'
/// LIMIT 65000` returned exactly 65,000 rows, no fewer. The defect there is simply that a
/// hard-coded `LIMIT` sized to a past graph silently becomes a sample: that same table holds
/// **358,829** NodeAdded rows, so the query read 18% of them and the caller could not tell.
///
/// So `kind` stays in the `WHERE` clause here — it is the efficient and correct path — and the
/// scan pages until the table is exhausted rather than stopping at a guessed ceiling.
pub async fn scan_events_by_kind(
    db: &Surreal<Any>,
    kind: &str,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    scan_events_by_kinds_paged(db, &[kind], SCAN_PAGE_SIZE).await
}

/// [`scan_events_by_kind`] over several kinds, replacing the `kind IN [...]` form.
pub async fn scan_events_by_kinds(
    db: &Surreal<Any>,
    kinds: &[&str],
) -> surrealdb::Result<Vec<serde_json::Value>> {
    scan_events_by_kinds_paged(db, kinds, SCAN_PAGE_SIZE).await
}

/// [`scan_events_by_kinds`] with an explicit page size, so tests can force multi-page behaviour.
pub async fn scan_events_by_kinds_paged(
    db: &Surreal<Any>,
    kinds: &[&str],
    page_size: usize,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    for kind in kinds {
        let mut start = 0usize;
        loop {
            let sql = format!(
                "SELECT id, event_id, kind, payload, timestamp FROM graph_events \
                 WHERE kind = $kind \
                 ORDER BY id LIMIT {page_size} START {start}"
            );
            let rows: Vec<serde_json::Value> =
                db.query(sql).bind(("kind", json!(kind))).await?.take(0)?;
            let returned = rows.len();
            if returned == 0 {
                break;
            }
            out.extend(rows);
            start += returned;
            if start >= SCAN_MAX_ROWS {
                tracing::warn!(
                    scanned = start,
                    cap = SCAN_MAX_ROWS,
                    kind = %kind,
                    "event scan hit the safety cap — results are truncated"
                );
                break;
            }
        }
    }
    Ok(out)
}

/// Every `graph_events` row of `kind` whose `payload.<field>` contains `needle`.
///
/// For the `MotifDetected … result_json CONTAINS $entity` shape, where the predicate is on a
/// payload field other than `weight_json`. Any display cap the caller wants should be applied to
/// the returned Vec — a `LIMIT` in the query is a cap on *what the planner happened to look at*,
/// not on the matches.
pub async fn scan_events_by_payload_contains(
    db: &Surreal<Any>,
    kind: &str,
    field: &str,
    needle: &str,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    if !is_plain_ident(field) {
        return Ok(Vec::new());
    }
    let rows = scan_events_by_kind(db, kind).await?;
    Ok(rows
        .into_iter()
        .filter(|r| {
            r["payload"][field]
                .as_str()
                .is_some_and(|v| v.contains(needle))
        })
        .collect())
}

/// Payload keys are compile-time literals at every call site; validated so a careless future
/// caller cannot turn one into an injection point.
fn is_plain_ident(field: &str) -> bool {
    !field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether any `graph_events` row of `kind` has `payload.<field>` containing `needle`.
///
/// Replaces the `… AND payload.result_json CONTAINS $hash LIMIT 1` existence checks. `LIMIT 1`
/// offers no protection: under the planner bug the query can return nothing for a row that
/// exists, which turns "have I already emitted this motif?" into "emit it again". Duplicate
/// motifs are harder to notice than missing ones, because nothing looks empty.
pub async fn event_payload_contains(
    db: &Surreal<Any>,
    kind: &str,
    field: &str,
    needle: &str,
) -> surrealdb::Result<bool> {
    // `field` is a caller-supplied payload key, never user input — the call sites pass a literal
    // ("result_json", "weight_json"). Validated anyway so this cannot become an injection point
    // if a future caller is careless.
    if !is_plain_ident(field) {
        return Ok(false);
    }
    let mut start = 0usize;
    loop {
        // `id` must be in the projection — SurrealDB 3.x rejects an ORDER BY idiom that is not
        // selected ("Missing order idiom `id` in statement selection").
        let sql = format!(
            "SELECT id, payload.{field} AS v FROM graph_events \
             WHERE kind = $kind \
             ORDER BY id LIMIT {SCAN_PAGE_SIZE} START {start}"
        );
        let rows: Vec<serde_json::Value> =
            db.query(sql).bind(("kind", json!(kind))).await?.take(0)?;
        let returned = rows.len();
        if returned == 0 {
            return Ok(false);
        }
        if rows
            .iter()
            .filter_map(|r| r["v"].as_str())
            .any(|v| v.contains(needle))
        {
            return Ok(true);
        }
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                kind = %kind,
                "existence scan hit the safety cap — a match beyond it would be missed"
            );
            return Ok(false);
        }
    }
}

/// [`scan_events_by_type`] with an explicit page size, so tests can force multi-page behaviour
/// without seeding [`SCAN_PAGE_SIZE`] rows.
pub async fn scan_events_by_type_paged(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
    page_size: usize,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut start = 0usize;
    loop {
        // `id` must appear in the projection — SurrealDB 3.x rejects an ORDER BY idiom that is
        // not selected.
        // Projects the same fields as the `SELECT *` this replaces — callers parse `payload` and
        // several read `timestamp`, so an abbreviated projection would silently change their
        // behaviour rather than fail to compile.
        let sql = format!(
            "SELECT id, event_id, kind, payload, timestamp FROM graph_events \
             WHERE payload.weight_json CONTAINS $marker \
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db
            .query(sql)
            .bind(("marker", json!(type_marker)))
            .await?
            .take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break;
        }
        out.extend(
            rows.into_iter()
                .filter(|r| kind == ANY_KIND || r["kind"].as_str() == Some(kind)),
        );
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                cap = SCAN_MAX_ROWS,
                marker = %type_marker,
                "event scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

/// The `weight_json` of every matching row, parsed. The common case for adapters that only need
/// the node payload and not the record id.
pub async fn scan_weights_by_type(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    let rows = scan_events_by_type(db, kind, type_marker).await?;
    Ok(rows
        .iter()
        .filter_map(|r| r["payload"]["weight_json"].as_str())
        .filter_map(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
        .collect())
}

/// Every matching row as `(entity_id, parsed weight_json)`.
///
/// `entity_id` is read through `type::string()` at the call site's request — SurrealDB 3.x
/// returns `payload.entity_id` as a RecordId for `director:*` style ids, so `as_str()` on the raw
/// value yields `""` and any comparison against it silently fails.
///
/// # This returns one entry per EVENT, not per entity
///
/// `graph_events` is an append-only log: re-ingesting an entity emits another `NodeAdded`, so the
/// same `entity_id` appears many times. **`.len()` on this vector is an event count, and using it
/// as a population is a measured defect, not a hypothetical.** On `an_anglesey_aebaa785`
/// (2026-08-31) it reads 105,613 for 45,874 distinct Contracts — a 2.3× inflation that produced a
/// false `Critical` "coverage REGRESSED 38.0% vs 95.0%" alarm in `identity_integrity`, found by
/// Phase 36's differential (`36-DIFFERENTIAL-RESULTS.md`, bucket (a)).
///
/// Use [`scan_latest_entities_by_type`] when you want entities. Use this one only when you
/// genuinely want every event.
pub async fn scan_entities_by_type(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
) -> surrealdb::Result<Vec<(String, serde_json::Value)>> {
    let rows = scan_events_by_type(db, kind, type_marker).await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let eid = entity_id_of(&r["payload"])?;
            let wj = r["payload"]["weight_json"].as_str()?;
            let weight = serde_json::from_str::<serde_json::Value>(wj).ok()?;
            Some((eid, weight))
        })
        .collect())
}

/// One `(entity_id, parsed weight_json)` per DISTINCT entity, keeping the event with the greatest
/// `timestamp`.
///
/// This is the population-safe counterpart to [`scan_entities_by_type`]: `.len()` here is a count
/// of entities, which is what a probe reporting a `population` almost always means.
///
/// # Why timestamp, and not scan order
///
/// `graph_events` record ids are random (`graph_events:0014aiz7k0s5t6zryryo`), so the `ORDER BY id`
/// the paged scan uses is **not chronological**. "Last row in scan order" is an arbitrary pick
/// among an entity's events, not its most recent write. That distinction is not academic: taking
/// the last-in-scan-order event for `identity_integrity` on `an_anglesey_aebaa785` reported 15,096
/// published Contracts where ordering by `timestamp` reports 37,856 — the provenance-bearing event
/// is simply not the one that happens to sort last.
///
/// Rows with no readable `timestamp` still participate (treated as `0`), so an entity whose events
/// all lack one still yields exactly one pair rather than disappearing from the population.
///
/// Output order is the first-appearance order of each `entity_id` in the scan, which is stable for
/// a given scan result and independent of the timestamps.
///
/// A caller needing the *earliest* event instead — `temporal_burst`, which buckets by ingestion
/// day and must not let a re-ingest re-date a Contract — dedups on its own, because it also needs
/// the raw row's `timestamp`, which this signature drops.
pub async fn scan_latest_entities_by_type(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
) -> surrealdb::Result<Vec<(String, serde_json::Value)>> {
    let rows = scan_events_by_type(db, kind, type_marker).await?;
    Ok(latest_entity_weights(rows))
}

/// Collapse raw `graph_events` rows to one `(entity_id, parsed weight_json)` per entity, keeping
/// the greatest `timestamp`.
///
/// Applies the same gate [`scan_entities_by_type`] does — a parseable `entity_id` AND a parseable
/// `weight_json` — so populations derived from the two stay comparable. Split out from
/// [`scan_latest_entities_by_type`] so the collapsing rule is unit-testable without a database.
///
/// # Do not unify this with the export's copy
///
/// `crates/cli/src/commands/export_graphml.rs` implements this same "greatest `timestamp` wins"
/// rule twice more — once for nodes in `load_from_events`, once for edges in `dedup_edge_rows`.
/// That duplication is **deliberate, and required by ORC-06**: the population oracle must not
/// route through `persistence::scan`, so that a defect in one implementation is *visible* as a
/// disagreement with the other rather than silently shared.
///
/// This is not theoretical. On 2026-08-27 the export's copy was correct while no copy existed
/// here, and three probes counted raw `graph_events` rows as Contracts — the export held 39,860
/// where the probe reported 99,599 (quick task `260831-ffx`). A shared implementation would have
/// been wrong in both places and agreed with itself.
///
/// Merging these to remove the duplication deletes the cross-check. Don't.
pub fn latest_entity_weights(rows: Vec<serde_json::Value>) -> Vec<(String, serde_json::Value)> {
    let mut order: Vec<String> = Vec::new();
    let mut best: std::collections::HashMap<String, (u64, serde_json::Value)> =
        std::collections::HashMap::new();

    for row in &rows {
        let Some(eid) = entity_id_of(&row["payload"]) else {
            continue;
        };
        let Some(wj) = row["payload"]["weight_json"].as_str() else {
            continue;
        };
        let Ok(weight) = serde_json::from_str::<serde_json::Value>(wj) else {
            continue;
        };
        // A missing timestamp sorts lowest rather than excluding the row — losing an entity from
        // the population entirely would be a worse failure than picking its only event.
        let ts = row["timestamp"].as_u64().unwrap_or(0);

        match best.get(&eid) {
            Some((best_ts, _)) if *best_ts >= ts => {}
            Some(_) => {
                best.insert(eid, (ts, weight));
            }
            None => {
                order.push(eid.clone());
                best.insert(eid, (ts, weight));
            }
        }
    }

    order
        .into_iter()
        .filter_map(|eid| best.remove(&eid).map(|(_, weight)| (eid, weight)))
        .collect()
}

/// `payload.entity_id` as a string, whether SurrealDB handed it back as a plain string or coerced
/// it into a RecordId object.
///
/// Public because [`scan_entities_by_type`] silently DROPS every row this returns `None` for (and
/// every row whose `weight_json` will not parse), so its row count is not `scan_events_by_type`'s.
/// A probe that needs the raw event — a `timestamp`, say — but must report a population directly
/// comparable to one derived from `scan_entities_by_type` has to apply the same gate, and cannot
/// do that without this function. `temporal_burst.rs` is the live caller; the divergence it
/// closes is a real reported defect, not a hypothetical.
pub fn entity_id_of(payload: &serde_json::Value) -> Option<String> {
    let raw = payload.get("entity_id")?;
    if let Some(s) = raw.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    // RecordId coercion: {"tb": "director", "id": {"String": "jane-smith"}} and near variants.
    let tb = raw.get("tb").and_then(|v| v.as_str())?;
    let id = raw.get("id")?;
    let id_str = id
        .as_str()
        .map(str::to_owned)
        .or_else(|| id.get("String").and_then(|v| v.as_str()).map(str::to_owned))
        .or_else(|| id.as_i64().map(|n| n.to_string()))?;
    Some(format!("{tb}:{id_str}"))
}

// ── `identity_links` / `land_flip_candidates` paged scans (PRB-03/PRB-04, GRD-04/GRD-05) ────
//
// Same rule as the `graph_events` scans above: the loop terminates only on an empty page, and
// ordering is always `ORDER BY id` — never a data column like `confidence`/`created_at` — because
// paging on a field with ties can skip or repeat rows across pages, and a DESC-ordered ceiling
// discards exactly the tail a review-queue probe exists to surface. `links.rs:52-56`'s
// `ORDER BY confidence DESC LIMIT 100` and `flips.rs:74-75`'s `ORDER BY created_at DESC LIMIT 100`
// are the live instances of this shape that these functions replace.

/// One `identity_links` row (`crates/schema/src/db.rs:258-270`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdentityLinkRow {
    pub id: String,
    pub source_node_id: String,
    pub target_node_id: String,
    pub resolution_type: String,
    pub confidence: f64,
    pub source: String,
    pub status: String,
    pub evidence_json: String,
    pub resolved_at: chrono::DateTime<chrono::Utc>,
}

/// `id`/`source_node_id`/`target_node_id` as a string, whether SurrealDB handed it back as a
/// plain string or coerced it into a RecordId object — the same mechanism as [`entity_id_of`],
/// operating on the value directly rather than under a `payload` key. A `director:`-keyed
/// `identity_links` row returns `source_node_id`/`target_node_id` as RecordId-shaped objects
/// despite the field being schema-typed `TYPE string` — SurrealDB's parser auto-coerces a value
/// that matches `table:key` syntax regardless of the declared field type
/// (`crates/core/src/persistence/traversal.rs:359-395` hit this first, for the same two fields).
pub(crate) fn record_id_str(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    let tb = v.get("tb").and_then(|x| x.as_str())?;
    let id = v.get("id")?;
    let id_str = id
        .as_str()
        .map(str::to_owned)
        .or_else(|| id.get("String").and_then(|x| x.as_str()).map(str::to_owned))
        .or_else(|| id.as_i64().map(|n| n.to_string()))?;
    Some(format!("{tb}:{id_str}"))
}

/// Builds an [`IdentityLinkRow`] from one raw `identity_links` row, tolerating RecordId
/// coercion on `id`/`source_node_id`/`target_node_id` via [`record_id_str`]. `resolution_type`/
/// `source`/`status`/`evidence_json` never take `table:key` shape, so plain string extraction is
/// safe for those; `confidence` is a plain float.
///
/// Returns `None` — and logs a warning — only when `id`/`source_node_id`/`target_node_id` are
/// unrecoverable, or `resolved_at` fails to parse: those are the fields where a parse failure
/// means the row is unusable, not merely missing an optional detail.
fn parse_identity_link_row(v: &serde_json::Value) -> Option<IdentityLinkRow> {
    let id = record_id_str(&v["id"]);
    let source_node_id = record_id_str(&v["source_node_id"]);
    let target_node_id = record_id_str(&v["target_node_id"]);
    let resolved_at = v["resolved_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    let (id, source_node_id, target_node_id, resolved_at) =
        match (id, source_node_id, target_node_id, resolved_at) {
            (Some(id), Some(s), Some(t), Some(r)) => (id, s, t, r),
            _ => {
                tracing::warn!(
                    raw_id = %v["id"],
                    "identity_links row unusable — id/source_node_id/target_node_id/resolved_at \
                     could not be parsed; skipping"
                );
                return None;
            }
        };

    Some(IdentityLinkRow {
        id,
        source_node_id,
        target_node_id,
        resolution_type: v["resolution_type"].as_str().unwrap_or_default().to_owned(),
        confidence: v["confidence"].as_f64().unwrap_or(0.0),
        source: v["source"].as_str().unwrap_or_default().to_owned(),
        status: v["status"].as_str().unwrap_or_default().to_owned(),
        evidence_json: v["evidence_json"].as_str().unwrap_or_default().to_owned(),
        resolved_at,
    })
}

/// Every `identity_links` row matching `status` and/or `source` (AND-ed when both are given),
/// paged past [`SCAN_PAGE_SIZE`] if needed.
///
/// `status`/`source` are never both `Some` in any of the 9 real call sites this mirrors
/// (Python's `fetch_identity_links`, the research notes' Q3), but AND-ing them costs nothing here and
/// avoids a second near-identical function the day a probe needs both.
pub async fn scan_identity_links(
    db: &Surreal<Any>,
    status: Option<&str>,
    source: Option<&str>,
) -> surrealdb::Result<Vec<IdentityLinkRow>> {
    scan_identity_links_paged(db, status, source, SCAN_PAGE_SIZE).await
}

/// [`scan_identity_links`] with an explicit page size, so tests can force multi-page behaviour.
///
/// Ordering is `ORDER BY id` — never `ORDER BY confidence` — see the module-level note above
/// this section (GRD-04). `status`/`source` are plain equality on indexed fields
/// (`idx_identity_links_status`, `idx_identity_links_source_node`,
/// `idx_identity_links_target_node`, `crates/schema/src/db.rs:267-269`) — never `CONTAINS` — so
/// this is not the Rule 1 planner-bug shape.
///
/// The page is deserialized as `Vec<serde_json::Value>` first, then mapped into
/// [`IdentityLinkRow`] via [`parse_identity_link_row`] — a single `director:`-keyed row in the
/// page would make the whole page's `.take(0)?` fail to decode if this deserialized directly
/// into `Vec<IdentityLinkRow>`.
pub async fn scan_identity_links_paged(
    db: &Surreal<Any>,
    status: Option<&str>,
    source: Option<&str>,
    page_size: usize,
) -> surrealdb::Result<Vec<IdentityLinkRow>> {
    let mut out: Vec<IdentityLinkRow> = Vec::new();
    let mut start = 0usize;
    loop {
        let mut clauses = Vec::new();
        if status.is_some() {
            clauses.push("status = $status");
        }
        if source.is_some() {
            clauses.push("source = $source");
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT id, source_node_id, target_node_id, resolution_type, confidence, source, \
             status, evidence_json, resolved_at FROM identity_links {where_clause} \
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db
            .query(sql)
            .bind(("status", status.map(str::to_owned)))
            .bind(("source", source.map(str::to_owned)))
            .await?
            .take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break; // the only valid termination — GRD-04
        }
        out.extend(rows.iter().filter_map(parse_identity_link_row));
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                "identity_links scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

/// Whether at least one `identity_links` row matches `status`/`source`.
///
/// Deliberately its own function rather than a `limit` parameter on [`scan_identity_links`], so
/// an existence check can never be mistaken for, or reported as, a count (the research notes' Q3's
/// explicit rationale — Pitfall 3's exact ambiguity). A bare equality-only `LIMIT 1` (no
/// `CONTAINS`) on indexed fields is safe from the planner-bug truncation shape.
pub async fn identity_link_exists(
    db: &Surreal<Any>,
    status: Option<&str>,
    source: Option<&str>,
) -> surrealdb::Result<bool> {
    let mut clauses = Vec::new();
    if status.is_some() {
        clauses.push("status = $status");
    }
    if source.is_some() {
        clauses.push("source = $source");
    }
    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    let sql = format!("SELECT id FROM identity_links {where_clause} LIMIT 1");
    let rows: Vec<serde_json::Value> = db
        .query(sql)
        .bind(("status", status.map(str::to_owned)))
        .bind(("source", source.map(str::to_owned)))
        .await?
        .take(0)?;
    Ok(!rows.is_empty())
}

/// Every `land_flip_candidates` row, optionally filtered by `status`.
pub async fn scan_land_flip_candidates(
    db: &Surreal<Any>,
    status: Option<&str>,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    scan_land_flip_candidates_paged(db, status, SCAN_PAGE_SIZE).await
}

/// [`scan_land_flip_candidates`] with an explicit page size, so tests can force multi-page
/// behaviour.
///
/// Ordering is `ORDER BY id`, not `created_at` — `created_at` is not indexed
/// (`crates/schema/src/db.rs:367-390`) and a newest-first ceiling on this review queue would
/// hide the oldest stale `pending_review` rows the same way `links.rs`'s confidence-DESC
/// ceiling hides the low-confidence tail (the research notes' Q2; live instance `flips.rs:74-75`).
///
/// Projects `*` — this table carries no `weight_json`-scale payload per row, so a full-row
/// projection is cheap, and rows carry no RecordId-coercion risk (no field stores another
/// table's `table:key` reference as a plain string) — `flips.rs` already consumes rows this way.
pub async fn scan_land_flip_candidates_paged(
    db: &Surreal<Any>,
    status: Option<&str>,
    page_size: usize,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut start = 0usize;
    loop {
        let where_clause = if status.is_some() {
            "WHERE status = $status"
        } else {
            ""
        };
        let sql = format!(
            "SELECT * FROM land_flip_candidates {where_clause} \
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db
            .query(sql)
            .bind(("status", status.map(str::to_owned)))
            .await?
            .take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break; // the only valid termination — GRD-04
        }
        out.extend(rows);
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                "land_flip_candidates scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

// ── `enrichment_runs` paged scan (PRB-05, GRD-04) ──────────────────────────────────────────
//
// The sole writer is `crates/enricher/src/engine.rs:414-437`'s `UPSERT enrichment_runs:{source}
// SET source = '{source}', last_run_at = $last_run_at, candidates_found = ..., auto_linked = ...,
// pending_review = ..., discarded = ...` — `last_run_at` is a plain UNIX-seconds `i64`, not a
// `datetime` field, so the datetime-coercion silent-drop risk does not apply here.
//
// Same rule as every other scan in this file: the loop terminates only on an empty page, and
// ordering is always `ORDER BY id` — this table holds at most ~15 rows in production today, but
// `crates/core/tests/query_shape_guard.rs`'s `WATCHED_TABLES` entry for `enrichment_runs` trips
// at threshold 1 on any bare `LIMIT`, so the reader must page regardless of current row count.

/// One `enrichment_runs` row — a per-source run marker (`crates/enricher/src/engine.rs:414-437`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunMarker {
    pub source: String,
    pub last_run_at: i64,
    pub candidates_found: i64,
    pub auto_linked: i64,
    pub pending_review: i64,
    pub discarded: i64,
}

/// Builds a [`RunMarker`] from one raw `enrichment_runs` row. `source` never takes `table:key`
/// shape (it's a plain adapter name like `"ch_psc"`, not a record reference), so plain string
/// extraction is safe — unlike `identity_links`' `source_node_id`/`target_node_id`, this field
/// needs no RecordId-coercion handling.
///
/// Returns `None` — and logs a warning — only when `source` is unrecoverable, since every other
/// field defaults safely to `0` for a row this reader would otherwise be unable to key at all.
fn parse_run_marker(v: &serde_json::Value) -> Option<RunMarker> {
    let Some(source) = v["source"].as_str().map(str::to_owned) else {
        tracing::warn!(
            raw_id = %v["id"],
            "enrichment_runs row unusable — source could not be parsed; skipping"
        );
        return None;
    };
    Some(RunMarker {
        source,
        last_run_at: v["last_run_at"].as_i64().unwrap_or(0),
        candidates_found: v["candidates_found"].as_i64().unwrap_or(0),
        auto_linked: v["auto_linked"].as_i64().unwrap_or(0),
        pending_review: v["pending_review"].as_i64().unwrap_or(0),
        discarded: v["discarded"].as_i64().unwrap_or(0),
    })
}

/// Every `enrichment_runs` row, keyed by `source`, paged past [`SCAN_PAGE_SIZE`] if needed.
pub async fn scan_enrichment_runs(
    db: &Surreal<Any>,
) -> surrealdb::Result<std::collections::HashMap<String, RunMarker>> {
    scan_enrichment_runs_paged(db, SCAN_PAGE_SIZE).await
}

/// [`scan_enrichment_runs`] with an explicit page size, so tests can force multi-page behaviour.
///
/// `id` is included in the SELECT projection even though the returned map never uses it —
/// SurrealDB 3.x rejects an `ORDER BY` idiom that is not itself selected (the same constraint
/// `scan_identity_links_paged` above observes).
pub async fn scan_enrichment_runs_paged(
    db: &Surreal<Any>,
    page_size: usize,
) -> surrealdb::Result<std::collections::HashMap<String, RunMarker>> {
    let mut out: std::collections::HashMap<String, RunMarker> = std::collections::HashMap::new();
    let mut start = 0usize;
    loop {
        let sql = format!(
            "SELECT id, source, last_run_at, candidates_found, auto_linked, pending_review, \
             discarded FROM enrichment_runs ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db.query(sql).await?.take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break; // the only valid termination — GRD-04
        }
        for row in &rows {
            if let Some(marker) = parse_run_marker(row) {
                out.insert(marker.source.clone(), marker);
            }
        }
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                "enrichment_runs scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

// ── Relation tables (PIV-06) ─────────────────────────────────────────────────
//
// The five relation tables the entity profile reads — `owns_property`, `offshore_link`,
// `trustee_of`, `donates_to` and `controls` — were each read with a bare `LIMIT 100000` and no
// `START`. That is rule 2 in this module's header, applied to a table the workspace's
// `query_shape_guard` does not watch, which is the only reason it survived GRD-09: the guard's
// `WATCHED_TABLES` list covers `graph_events` and `identity_links`, so a 100,000-row ceiling on
// `owns_property` was invisible to it rather than accepted by it.
//
// The ceiling is not hypothetical. `owns_property` holds roughly 100,000 OCOD edges at GB scale
// — the site's own comment says so — so the profile's property section was one ingest away from
// sampling the table and reporting the sample as the entity's holdings. PIV-06's requirement is
// that no literal decides what an analyst sees, so these reads page like every other scan here:
// `ORDER BY id`, `START` advanced by rows *actually received*, terminating only on an empty page.

/// Page size for relation-table scans. Relation rows are small (two record ids plus at most a
/// couple of scalar fields), so this can be larger than [`SCAN_PAGE_SIZE`]'s `weight_json`-sized
/// page without risking the response truncation described at the top of this module.
pub const RELATION_PAGE_SIZE: usize = 20_000;

/// Which end of a relation edge a filter applies to.
///
/// `owns_property`, `offshore_link`, `trustee_of` and `donates_to` are all declared
/// `RELATION FROM entity TO …`, so the profiled entity sits on the `in` side; `controls` is
/// `RELATION FROM psc TO entity`, so it sits on the `out` side. Making the side an explicit
/// argument is what lets one scan serve both without a call site quietly reading the wrong end —
/// the false-attribution guard T-22-01-03 exists because reading the wrong side puts another
/// entity's edges on this entity's profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationSide {
    /// The `in` (left / source) side — `type::string(in)`.
    In,
    /// The `out` (right / destination) side — `type::string(out)`.
    Out,
}

impl RelationSide {
    fn field(self) -> &'static str {
        match self {
            RelationSide::In => "in",
            RelationSide::Out => "out",
        }
    }
}

/// Every row of relation `table`, projected as `in_str`/`out_str` plus `extra_fields`, paged to
/// exhaustion.
///
/// `extra_fields` are `(path, alias)` pairs — `("data.price_paid", "price_paid")` projects
/// `data.price_paid AS price_paid`. Both halves are validated as dotted plain identifiers, so a
/// future caller cannot turn the projection into an injection point; every call site today passes
/// compile-time literals.
///
/// `filter` restricts the scan to one end of the edge in SQL (`type::string(in) = $key`), which is
/// how the `owns_property` read avoids dragging GB-scale OCOD edges across the wire for every
/// profile. Passing `None` reads the whole table and leaves filtering to the caller — the shape the
/// four smaller tables use, because their stored `in` values need the caller's own quote-trimming
/// before they compare equal.
///
/// An invalid `table` or projection yields an empty result and a logged error rather than an
/// interpolated query: this is a validation failure in the *caller's* literals, not in data.
pub async fn scan_relation_rows(
    db: &Surreal<Any>,
    table: &str,
    extra_fields: &[(&str, &str)],
    filter: Option<(RelationSide, &str)>,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    scan_relation_rows_paged(db, table, extra_fields, filter, RELATION_PAGE_SIZE).await
}

/// [`scan_relation_rows`] with an explicit page size, so tests can force multi-page behaviour
/// without seeding [`RELATION_PAGE_SIZE`] rows.
pub async fn scan_relation_rows_paged(
    db: &Surreal<Any>,
    table: &str,
    extra_fields: &[(&str, &str)],
    filter: Option<(RelationSide, &str)>,
    page_size: usize,
) -> surrealdb::Result<Vec<serde_json::Value>> {
    if !is_plain_ident(table) {
        tracing::error!(table = %table, "relation scan refused: table name is not a plain ident");
        return Ok(Vec::new());
    }
    let mut projection =
        String::from("id, type::string(in) AS in_str, type::string(out) AS out_str");
    for (path, alias) in extra_fields {
        if !is_dotted_ident(path) || !is_plain_ident(alias) {
            tracing::error!(
                table = %table,
                path = %path,
                alias = %alias,
                "relation scan refused: projection field is not a plain ident"
            );
            return Ok(Vec::new());
        }
        projection.push_str(&format!(", {path} AS {alias}"));
    }
    let where_clause = match filter {
        Some((side, _)) => format!("WHERE type::string({}) = $side_key ", side.field()),
        None => String::new(),
    };
    let side_key = filter.map(|(_, key)| key.to_owned());

    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut start = 0usize;
    loop {
        // `id` must appear in the projection — SurrealDB 3.x rejects an ORDER BY idiom that is
        // not selected ("Missing order idiom `id` in statement selection").
        let sql = format!(
            "SELECT {projection} FROM {table} {where_clause}\
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db
            .query(sql)
            .bind(("side_key", side_key.clone()))
            .await?
            .take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break; // the only valid termination — GRD-04
        }
        out.extend(rows);
        start += returned;
        if start >= SCAN_MAX_ROWS {
            tracing::warn!(
                scanned = start,
                cap = SCAN_MAX_ROWS,
                table = %table,
                "relation scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

/// A dotted path of plain identifiers — `data.price_paid`, `tenure`. Empty segments rejected.
fn is_dotted_ident(path: &str) -> bool {
    !path.is_empty() && path.split('.').all(is_plain_ident)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_string_entity_id_passes_through() {
        let p = json!({"entity_id": "GB-COH:12345678"});
        assert_eq!(entity_id_of(&p).as_deref(), Some("GB-COH:12345678"));
    }

    #[test]
    fn a_coerced_record_id_is_reassembled() {
        // What SurrealDB 3.x actually returns for `director:jane-smith`. Read naively with
        // `as_str()` this is `""`, which is why every comparison against it silently fails.
        let p = json!({"entity_id": {"tb": "director", "id": {"String": "jane-smith"}}});
        assert_eq!(entity_id_of(&p).as_deref(), Some("director:jane-smith"));
    }

    #[test]
    fn a_record_id_with_a_bare_string_id_is_reassembled() {
        let p = json!({"entity_id": {"tb": "director", "id": "jane-smith"}});
        assert_eq!(entity_id_of(&p).as_deref(), Some("director:jane-smith"));
    }

    #[test]
    fn an_empty_or_missing_entity_id_is_none() {
        assert_eq!(entity_id_of(&json!({"entity_id": ""})), None);
        assert_eq!(entity_id_of(&json!({})), None);
    }

    // ── against a real server ────────────────────────────────────────────────────────────────
    //
    // The whole point of this module is a defect that only manifests against a real indexed
    // table, so a unit test cannot reach it. This seeds enough rows to cross a page boundary and
    // asserts the scan returns every one.
    use surrealdb::opt::auth::Root;

    fn db_tests_enabled() -> bool {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return false;
        }
        std::net::TcpStream::connect("[::1]:8000").is_ok()
            || std::net::TcpStream::connect("127.0.0.1:8000").is_ok()
    }

    /// Returns the handle plus the namespace guard; callers must bind the
    /// guard so the namespace outlives the test.
    async fn scratch_db(suffix: &str) -> (Surreal<Any>, urban_integrity_test_support::NsGuard) {
        let guard = urban_integrity_test_support::NsGuard::with_prefix("test_core_scan");
        let db: Surreal<Any> = Surreal::init();
        db.connect("http://localhost:8000").await.expect("connect");
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await
        .expect("signin");
        db.use_ns(guard.name())
            .use_db(format!("scan_{suffix}"))
            .await
            .expect("use ns/db");
        db.query("REMOVE TABLE IF EXISTS graph_events")
            .await
            .expect("reset");
        db.query("DEFINE TABLE graph_events SCHEMALESS")
            .await
            .expect("define");
        // The index is what triggers the planner bug — without it the broken shape would work.
        db.query("DEFINE INDEX idx_graph_events_kind ON graph_events FIELDS kind")
            .await
            .expect("define index");
        (db, guard)
    }

    #[tokio::test]
    async fn db_scan_returns_every_row_where_the_indexed_shape_truncates() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = scratch_db("truncation").await;

        const N: usize = 250;
        for i in 0..N {
            db.query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = $e, \
                 payload = { entity_id: $id, weight_json: $w }",
            )
            .bind(("e", format!("evt-{i}")))
            .bind(("id", format!("GB-COH:{i:08}")))
            .bind((
                "w",
                format!("{{\"type\":\"Officer\",\"name\":\"Officer {i}\"}}"),
            ))
            .await
            .expect("seed");
        }
        // Decoys that must not come back: right marker, wrong kind.
        for i in 0..10 {
            db.query(
                "CREATE graph_events SET kind = 'NodeUpdated', event_id = $e, \
                 payload = { entity_id: $id, weight_json: '{\"type\":\"Officer\"}' }",
            )
            .bind(("e", format!("upd-{i}")))
            .bind(("id", format!("GB-COH:9{i:07}")))
            .await
            .expect("seed decoy");
        }

        // Page size below N so the paging path is exercised, not just the first page.
        let rows = scan_events_by_type_paged(&db, "NodeAdded", "\"type\":\"Officer\"", 40)
            .await
            .expect("scan");
        assert_eq!(
            rows.len(),
            N,
            "the scan must return every seeded row across page boundaries"
        );
        assert!(
            rows.iter().all(|r| r["kind"] == "NodeAdded"),
            "rows of another kind leaked through the Rust-side filter"
        );

        let entities = scan_entities_by_type(&db, "NodeAdded", "\"type\":\"Officer\"")
            .await
            .expect("scan entities");
        assert_eq!(entities.len(), N);
        assert!(entities
            .iter()
            .all(|(id, w)| id.starts_with("GB-COH:") && w["type"] == "Officer"));
    }

    #[tokio::test]
    async fn db_scan_by_kind_pages_past_a_hard_coded_ceiling() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = scratch_db("by_kind").await;

        const N: usize = 150;
        for i in 0..N {
            db.query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = $e, \
                 payload = { entity_id: $id, weight_json: '{\"type\":\"Entity\"}' }",
            )
            .bind(("e", format!("evt-{i}")))
            .bind(("id", format!("GB-COH:{i:08}")))
            .await
            .expect("seed");
        }
        for i in 0..7 {
            db.query(
                "CREATE graph_events SET kind = 'EdgeAdded', event_id = $e, \
                 payload = { weight_json: '{\"type\":\"AwardedTo\"}' }",
            )
            .bind(("e", format!("edge-{i}")))
            .await
            .expect("seed edge");
        }

        // This is the live `bfs_traversal` failure in miniature: a ceiling below the data.
        let capped: Vec<serde_json::Value> = db
            .query("SELECT payload FROM graph_events WHERE kind = 'NodeAdded' LIMIT 40")
            .await
            .expect("capped")
            .take(0)
            .expect("take");
        assert_eq!(
            capped.len(),
            40,
            "the capped form returns its ceiling, not the truth — this is what the fix replaces"
        );

        let all = scan_events_by_kinds_paged(&db, &["NodeAdded"], 40)
            .await
            .expect("scan by kind");
        assert_eq!(all.len(), N, "the paged scan must read past the page size");
        assert!(all.iter().all(|r| r["kind"] == "NodeAdded"));

        let both = scan_events_by_kinds_paged(&db, &["NodeAdded", "EdgeAdded"], 40)
            .await
            .expect("scan two kinds");
        assert_eq!(
            both.len(),
            N + 7,
            "the `kind IN [...]` replacement must cover every kind"
        );
    }

    #[tokio::test]
    async fn db_existence_check_finds_a_match_beyond_the_first_page() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = scratch_db("exists").await;

        // The needle is deliberately the LAST row seeded. A `LIMIT 1` existence check that
        // happens to look at the wrong rows reports "not present" and the caller re-emits.
        const N: usize = 120;
        for i in 0..N {
            let payload = if i == N - 1 {
                "{\"hash\":\"needle-abc123\"}".to_string()
            } else {
                format!("{{\"hash\":\"other-{i}\"}}")
            };
            db.query(
                "CREATE graph_events SET kind = 'MotifDetected', event_id = $e, \
                 payload = { result_json: $r }",
            )
            .bind(("e", format!("motif-{i}")))
            .bind(("r", payload))
            .await
            .expect("seed");
        }

        assert!(
            event_payload_contains(&db, "MotifDetected", "result_json", "needle-abc123")
                .await
                .expect("exists"),
            "an existing motif must be found, or the caller emits a duplicate"
        );
        assert!(
            !event_payload_contains(&db, "MotifDetected", "result_json", "absent-zzz")
                .await
                .expect("exists"),
            "a motif that was never emitted must not be reported as present"
        );
        assert!(
            !event_payload_contains(&db, "NodeAdded", "result_json", "needle-abc123")
                .await
                .expect("exists"),
            "the kind filter must still apply — a match of another kind is not a match"
        );
    }

    #[tokio::test]
    async fn db_any_type_scan_covers_every_marker_without_duplicating_rows() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = scratch_db("any_type").await;

        for i in 0..30 {
            db.query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = $e, \
                 payload = { entity_id: $id, weight_json: '{\"type\":\"Officer\"}' }",
            )
            .bind(("e", format!("off-{i}")))
            .bind(("id", format!("OFF:{i}")))
            .await
            .expect("seed officer");
        }
        for i in 0..20 {
            db.query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = $e, \
                 payload = { entity_id: $id, weight_json: '{\"type\":\"Entity\"}' }",
            )
            .bind(("e", format!("ent-{i}")))
            .bind(("id", format!("GB-COH:{i:08}")))
            .await
            .expect("seed entity");
        }

        let rows = scan_events_by_any_type(
            &db,
            "NodeAdded",
            &["\"type\":\"Officer\"", "\"type\":\"Entity\""],
        )
        .await
        .expect("scan any");
        assert_eq!(
            rows.len(),
            50,
            "the OR form must return the union of both markers"
        );

        let ids: std::collections::HashSet<String> =
            rows.iter().map(|r| r["id"].to_string()).collect();
        assert_eq!(
            ids.len(),
            50,
            "a row matching two markers must not be returned twice"
        );
    }

    // ── identity_links / land_flip_candidates paging (PRB-04, GRD-04) ──────────────────────
    //
    // These use `urban_integrity_test_support::TestNs`, not this file's own `scratch_db` —
    // `TestNs` applies the full `SCHEMA_DEF` (identity_links'/land_flip_candidates' indexes
    // included) in one call and is the preferred convention over this file's bespoke
    // graph_events-only helper (the pattern notes).

    /// Seeds one `identity_links` row with an explicit record key, so `ORDER BY id ASC`
    /// ordering is deterministic and independent of SurrealDB's default id-generation scheme.
    async fn seed_identity_link(
        db: &Surreal<Any>,
        key: &str,
        source_node_id: &str,
        target_node_id: &str,
        confidence: f64,
        source: &str,
        status: &str,
    ) {
        db.query(
            "CREATE type::record('identity_links', $key) SET \
             source_node_id = $src, target_node_id = $tgt, resolution_type = 'exact_ch_number', \
             confidence = $conf, source = $source, resolved_at = $resolved_at, \
             evidence_json = '{}', status = $status",
        )
        .bind(("key", key.to_string()))
        .bind(("src", source_node_id.to_string()))
        .bind(("tgt", target_node_id.to_string()))
        .bind(("conf", confidence))
        .bind(("source", source.to_string()))
        .bind(("status", status.to_string()))
        .bind(("resolved_at", chrono::Utc::now()))
        .await
        .expect("seed identity_links row")
        .check()
        .expect("seed identity_links row check");
    }

    #[tokio::test]
    async fn db_identity_links_scan_pages_past_a_hard_coded_ceiling() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_identity_scan",
            "identity_links_paging",
        )
        .await;

        // 149 filler rows, all with confidence well above the marker's, keyed so their ids sort
        // BEFORE the marker's under `ORDER BY id ASC`.
        const N: usize = 150;
        for i in 0..(N - 1) {
            seed_identity_link(
                &db,
                &format!("row_{i:04}"),
                &format!("GB-COH:{i:08}"),
                &format!("director:filler-{i}"),
                0.20 + (i as f64 * 0.001),
                "ch_psc",
                "pending_review",
            )
            .await;
        }
        // ⚠ PLAN-REVIEW CORRECTION: the marker — the lowest-confidence row, i.e. exactly what the
        // old `ORDER BY confidence DESC LIMIT 40` shape discards — is seeded LAST with a key
        // (`row_9999`) that sorts after every filler row's key under `ORDER BY id ASC`, so it
        // lands on the scan's FINAL page. Seeding it first would place it on page 1, where even a
        // paging implementation that incorrectly stops after the first page would still find it,
        // making the test unable to prove the loop reads to the end. Also director:-keyed, to
        // exercise the RecordId-coercion path on source_node_id.
        seed_identity_link(
            &db,
            "row_9999",
            "director:jane-marker-smith",
            "GB-COH:99999999",
            0.05,
            "ch_psc",
            "pending_review",
        )
        .await;

        // OLD shape: ORDER BY confidence DESC LIMIT 40 must under-report AND must not include
        // the marker — this is the direct reproduction of links.rs's ordering-bias defect.
        let capped: Vec<serde_json::Value> = db
            .query(
                "SELECT confidence FROM identity_links WHERE status = 'pending_review' \
                 ORDER BY confidence DESC LIMIT 40",
            )
            .await
            .expect("capped")
            .take(0)
            .expect("take");
        assert_eq!(
            capped.len(),
            40,
            "the capped form returns its ceiling, not the truth"
        );
        assert!(
            capped
                .iter()
                .all(|r| r["confidence"].as_f64() != Some(0.05)),
            "the old confidence-DESC-capped shape must have discarded the lowest-confidence row"
        );

        // NEW shape: the paged scan must read past the forced page size and return all 150,
        // including the marker, proving ORDER BY id paging — not ORDER BY confidence — is what
        // makes the scan complete.
        let all = scan_identity_links_paged(&db, Some("pending_review"), None, 40)
            .await
            .expect("scan");
        assert_eq!(all.len(), N, "the paged scan must read past the page size");
        let marker = all
            .iter()
            .find(|r| r.confidence == 0.05)
            .expect("the lowest-confidence row must be present in the paged scan result");
        assert_eq!(
            marker.source_node_id, "director:jane-marker-smith",
            "a director:-keyed source_node_id must round-trip correctly, not as an empty string"
        );

        // Independent count() assertion (ROADMAP criterion 2) — page-position-agnostic, catches
        // truncation wherever the marker lands, but is not a substitute for the placement above.
        let counted: Vec<serde_json::Value> = db
            .query("SELECT count() FROM identity_links WHERE status = 'pending_review' GROUP ALL")
            .await
            .expect("count")
            .take(0)
            .expect("take count");
        let count = counted
            .first()
            .and_then(|r| r["count"].as_i64())
            .unwrap_or(0);
        assert_eq!(
            count, N as i64,
            "independent count() must match the paged scan's len() exactly"
        );
        assert_eq!(count as usize, all.len());
    }

    #[tokio::test]
    async fn db_identity_links_scan_filters_by_status_and_source() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_identity_scan",
            "identity_links_filter",
        )
        .await;

        for i in 0..5 {
            seed_identity_link(
                &db,
                &format!("auto_ch_{i}"),
                &format!("GB-COH:{i:08}"),
                &format!("director:auto-ch-{i}"),
                0.9,
                "ch_psc",
                "auto_linked",
            )
            .await;
        }
        for i in 0..5 {
            seed_identity_link(
                &db,
                &format!("pending_ch_{i}"),
                &format!("GB-COH:1{i:07}"),
                &format!("director:pending-ch-{i}"),
                0.6,
                "ch_psc",
                "pending_review",
            )
            .await;
        }
        for i in 0..5 {
            seed_identity_link(
                &db,
                &format!("auto_ec_{i}"),
                &format!("GB-COH:2{i:07}"),
                &format!("director:auto-ec-{i}"),
                0.9,
                "electoral_commission",
                "auto_linked",
            )
            .await;
        }

        let by_status = scan_identity_links(&db, Some("auto_linked"), None)
            .await
            .expect("scan by status");
        assert_eq!(by_status.len(), 10, "status filter must match both sources");

        let by_source = scan_identity_links(&db, None, Some("ch_psc"))
            .await
            .expect("scan by source");
        assert_eq!(
            by_source.len(),
            10,
            "source filter must match both statuses"
        );

        let both = scan_identity_links(&db, Some("auto_linked"), Some("electoral_commission"))
            .await
            .expect("scan by status AND source");
        assert_eq!(both.len(), 5, "status+source must AND, not OR");
    }

    #[tokio::test]
    async fn db_identity_link_exists_finds_a_match_beyond_the_first_page() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_identity_scan",
            "identity_links_exists",
        )
        .await;

        // The needle is deliberately the LAST row seeded — mirrors
        // db_existence_check_finds_a_match_beyond_the_first_page above.
        const N: usize = 120;
        for i in 0..N {
            let source = if i == N - 1 {
                "needle_source"
            } else {
                "other_source"
            };
            seed_identity_link(
                &db,
                &format!("exists_{i:04}"),
                &format!("GB-COH:{i:08}"),
                &format!("director:exists-{i}"),
                0.5,
                source,
                "pending_review",
            )
            .await;
        }

        assert!(
            identity_link_exists(&db, None, Some("needle_source"))
                .await
                .expect("exists"),
            "a match placed last must still be found"
        );
        assert!(
            !identity_link_exists(&db, None, Some("absent_source"))
                .await
                .expect("exists"),
            "a source that was never seeded must not be reported as present"
        );
    }

    #[tokio::test]
    async fn db_land_flip_candidates_scan_pages_past_a_hard_coded_ceiling() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_identity_scan",
            "land_flip_paging",
        )
        .await;

        const N: usize = 150;
        for i in 0..N {
            db.query(
                "CREATE type::record('land_flip_candidates', $key) SET \
                 tier = 'tier_1', severity = 'MEDIUM', status = 'pending_review', \
                 buyer_identity_confirmed = false, date_source = 'ppd', \
                 ppd_transaction_id = $txn, ppd_postcode = 'AB1 2CD', ppd_paon = '12', \
                 ppd_street = 'HIGH STREET', ppd_price_gbp = 100000, \
                 ppd_date_of_transfer = $created_at, planning_ref = $planning_ref, \
                 created_at = $created_at",
            )
            .bind(("key", format!("flip_{i:04}")))
            .bind(("txn", format!("TXN-{i:06}")))
            .bind(("planning_ref", format!("PLAN-{i:06}")))
            .bind(("created_at", chrono::Utc::now()))
            .await
            .expect("seed land_flip_candidates row")
            .check()
            .expect("seed land_flip_candidates row check");
        }

        // `created_at` must be in the projection — SurrealDB 3.x rejects an ORDER BY idiom
        // that is not selected (same rule documented at event_payload_contains, above).
        let capped: Vec<serde_json::Value> = db
            .query(
                "SELECT id, created_at FROM land_flip_candidates WHERE status = 'pending_review' \
                 ORDER BY created_at DESC LIMIT 40",
            )
            .await
            .expect("capped")
            .take(0)
            .expect("take");
        assert_eq!(
            capped.len(),
            40,
            "the capped form returns its ceiling, not the truth"
        );

        let all = scan_land_flip_candidates_paged(&db, Some("pending_review"), 40)
            .await
            .expect("scan");
        assert_eq!(all.len(), N, "the paged scan must read past the page size");
    }

    // ── latest_entity_weights (the Contract-population fix) ─────────────────────────────────

    fn ev(eid: &str, ts: u64, marker: &str) -> serde_json::Value {
        serde_json::json!({
            "timestamp": ts,
            "payload": {
                "entity_id": eid,
                "weight_json": serde_json::json!({ "marker": marker }).to_string(),
            }
        })
    }

    /// Three events over two entities collapse to two pairs, and the event with the greatest
    /// `timestamp` wins — even when it is not the last in scan order.
    ///
    /// Regression pin for the bucket-(a) defect Phase 36's differential found: three probes were
    /// counting `scan_entities_by_type`'s per-event vector as a population, reading 105,613 for
    /// 45,874 distinct Contracts on the live corpus. The ordering half is pinned too, because the
    /// first attempt at this fix kept the last row in scan order and `graph_events` ids are random
    /// — that reported 15,096 published Contracts against the 37,856 the timestamp order gives.
    #[test]
    fn latest_entity_weights_keeps_one_per_entity_and_the_greatest_timestamp_wins() {
        // "newest" is deliberately NOT last in scan order.
        let rows = vec![
            ev("planning:a", 100, "older"),
            ev("planning:a", 900, "newest"),
            ev("planning:b", 500, "only"),
            ev("planning:a", 300, "middle"),
        ];

        let out = latest_entity_weights(rows);

        assert_eq!(
            out.len(),
            2,
            "four events over two entities must collapse to two pairs -- counting the raw vector \
             is an event count, not a population"
        );
        let a = out
            .iter()
            .find(|(eid, _)| eid == "planning:a")
            .expect("planning:a must survive");
        assert_eq!(
            a.1["marker"], "newest",
            "the greatest timestamp must win, not the last row in scan order -- graph_events ids \
             are random, so scan order is not chronological"
        );
    }

    /// An entity whose events carry no readable `timestamp` still yields exactly one pair rather
    /// than dropping out of the population.
    #[test]
    fn latest_entity_weights_keeps_entities_whose_events_have_no_timestamp() {
        let rows = vec![
            serde_json::json!({
                "payload": {
                    "entity_id": "planning:a",
                    "weight_json": "{\"marker\":\"untimed\"}",
                }
            }),
            ev("planning:b", 7, "timed"),
        ];

        let out = latest_entity_weights(rows);

        assert_eq!(
            out.len(),
            2,
            "a missing timestamp must not delete the entity"
        );
    }

    /// Output order is first-appearance order of each entity, independent of timestamps, so the
    /// result is deterministic for a given scan.
    #[test]
    fn latest_entity_weights_returns_first_appearance_order() {
        let rows = vec![
            ev("planning:a", 1, "a"),
            ev("planning:b", 999, "b"),
            ev("planning:a", 2, "a2"),
            ev("planning:c", 50, "c"),
        ];

        let ids: Vec<String> = latest_entity_weights(rows)
            .into_iter()
            .map(|(eid, _)| eid)
            .collect();

        assert_eq!(ids, vec!["planning:a", "planning:b", "planning:c"]);
    }

    /// Rows failing the parse gate are dropped, matching `scan_entities_by_type`'s own gate so
    /// populations derived from the two remain comparable.
    #[test]
    fn latest_entity_weights_applies_the_same_parse_gate() {
        let rows = vec![
            ev("planning:a", 1, "ok"),
            serde_json::json!({ "timestamp": 2, "payload": { "entity_id": "planning:b" } }),
            serde_json::json!({
                "timestamp": 3,
                "payload": { "entity_id": "planning:c", "weight_json": "not json" }
            }),
        ];

        let out = latest_entity_weights(rows);

        assert_eq!(
            out.len(),
            1,
            "unparseable rows must be dropped by the same gate"
        );
        assert_eq!(out[0].0, "planning:a");
    }
}
