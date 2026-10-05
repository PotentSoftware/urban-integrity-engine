use anyhow::Result;
use serde_json::json;
use uuid::Uuid;

pub mod anomaly_sink;
pub mod cessation;

/// The event-log table every backfill writes through.
const GRAPH_EVENTS_TABLE: &str = "graph_events";
pub mod ch;
pub mod cursor;
pub mod officers;
pub mod planning;
pub mod require;
pub mod session;
pub mod ward;

pub use cursor::{CursorState, ResumeCursor};
pub use session::{DbSession, SessionCreds};

#[derive(Debug, Default)]
pub struct BackfillStats {
    pub scanned: usize,
    pub matched: usize,
    pub skipped: usize,
    /// Mutation-only `applicant_provenance = Published` stamps applied to Contracts that were
    /// already correctly attributed (planning backfill, step 7). Zero new `graph_events` rows are
    /// inserted for these. `ch.rs`/`ward.rs` construct via `Default` and never populate this.
    pub bulk_stamped: usize,
    /// Candidates skipped by the planning backfill's edge-existence guard because a previous pass
    /// already inserted the real `AwardedTo` edge. The placeholder edge is deliberately never
    /// deleted, so a resolved Contract stays in the scan set forever and a re-run re-selects it;
    /// this counter is how a re-entrant run reports "nothing left to do here".
    /// `ch.rs`/`ward.rs` construct via `Default` and never populate this.
    pub already_resolved: usize,
    pub errors: usize,
}

/// Select a namespace/database in a test, retrying on write conflict.
///
/// Selecting a namespace/database that does not exist yet CREATES it, and the test harness runs
/// tests in parallel — so two tests racing to create the same namespace produce
/// `Transaction conflict: Write conflict, retry the transaction`. That is what the error asks for,
/// so retry with backoff.
///
/// This bites only from a clean SurrealDB: once the namespace exists, the race disappears and the
/// tests pass locally, which is why it surfaced in CI rather than on a dev machine.
#[cfg(test)]
pub(crate) async fn use_ns_db_with_retry(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    ns: &str,
    db_name: &str,
) {
    let mut last_err = None;
    for attempt in 0..10 {
        match db.use_ns(ns).use_db(db_name).await {
            Ok(_) => return,
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt + 1))).await;
            }
        }
    }
    panic!(
        "use ns/db {ns}/{db_name} failed after retries: {}",
        last_err.expect("loop always records an error before falling through")
    );
}

/// Page size for full-table event scans. Large enough that the round-trip count stays low,
/// small enough that one page's `weight_json` payload does not dominate the HTTP response.
pub(crate) const SCAN_PAGE_SIZE: usize = 20_000;

/// Safety cap on a single scan, so a pathological page sequence cannot loop forever. Well above
/// any real event count for one node/edge type; hitting it is logged as a truncation.
const SCAN_MAX_ROWS: usize = 2_000_000;

/// Scan every `graph_events` row whose `weight_json` carries `type_marker`, returning only those
/// with the requested `kind`.
///
/// Two defects are being avoided here, and both silently under-report rather than failing:
///
/// 1. **The planner bug.** `kind` is indexed (`idx_graph_events_kind`). Pairing that indexed
///    equality with a `CONTAINS` scan on the unindexed `payload.weight_json` makes SurrealDB 3.x
///    take the index path and return a truncated, run-to-run non-deterministic row set, while
///    `count()` on the identical predicate stays correct. So `kind` is deliberately NOT in the
///    `WHERE` clause — it is filtered in Rust below. The repo pins this as a regression in
///    `crates/core/tests/surreal_integration.rs`.
/// 2. **Response truncation.** A single large `LIMIT` returns well below the requested count when
///    `weight_json` is big, so a naive `LIMIT 50000` samples a fraction of the table and reports
///    success. `START` therefore advances by the number of rows *actually received*, which resumes
///    exactly where a truncated page stopped, and the loop ends only on an empty page — a short
///    page means truncation, not the end of the data.
///
/// Live measurement that prompted this (2026-07-28, `an_anglesey_aebaa785`): the previous
/// `kind = 'NodeAdded' AND … CONTAINS … LIMIT 50000` form returned **6,033 rows where `count()`
/// reported 33,046**, so the CH backfill had been running against ~18% of its input set while its
/// own `rows.len() == 50000` guard never fired.
pub(crate) async fn scan_events_by_type(
    session: &DbSession,
    kind: &str,
    type_marker: &str,
) -> Result<Vec<serde_json::Value>> {
    scan_events_by_type_paged(session, kind, type_marker, SCAN_PAGE_SIZE).await
}

/// [`scan_events_by_type`] with an explicit page size, so tests can force multi-page behaviour
/// without seeding `SCAN_PAGE_SIZE` rows.
pub(crate) async fn scan_events_by_type_paged(
    session: &DbSession,
    kind: &str,
    type_marker: &str,
    page_size: usize,
) -> Result<Vec<serde_json::Value>> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut start = 0usize;
    loop {
        // `id` must appear in the projection — SurrealDB 3.x rejects an ORDER BY idiom that is
        // not selected.
        let sql = format!(
            "SELECT id, event_id, kind, timestamp, payload FROM graph_events \
             WHERE payload.weight_json CONTAINS $marker \
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = session
            .query(&sql, &[("marker", json!(type_marker))])
            .await?
            .take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break;
        }
        out.extend(
            rows.into_iter()
                .filter(|r| r["kind"].as_str() == Some(kind)),
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

/// Remove the quoting SurrealDB adds around a record key that is not a bare identifier.
///
/// Both the backtick form (``foo:`a-b` ``) and the angle-bracket form (`foo:⟨a-b⟩`) are
/// unwrapped. Only a matching pair is stripped, so a key that genuinely contains one of
/// these characters is left alone rather than silently truncated.
pub(crate) fn strip_record_key_quoting(key: &str) -> &str {
    if let Some(inner) = key.strip_prefix('`').and_then(|k| k.strip_suffix('`')) {
        return inner;
    }
    if let Some(inner) = key
        .strip_prefix('\u{27e8}')
        .and_then(|k| k.strip_suffix('\u{27e9}'))
    {
        return inner;
    }
    key
}

/// Update the weight_json of an existing NodeAdded row addressed by its SurrealDB **record id**
/// (e.g. `graph_events:7hcaah36syb6inbwur0d`), not by `event_id`.
///
/// Prefer this over [`update_node_event`] wherever the caller already has the record id from a
/// scan. Two reasons, both measured live against `an_anglesey_aebaa785` (725k rows):
///
/// 1. **Correctness.** `update_node_event`'s `WHERE event_id = $eid AND kind = 'NodeAdded'` pairs
///    the indexed `kind` equality with an equality on an unindexed field. SurrealDB 3.x takes the
///    `idx_graph_events_kind` path and matches nothing: the same predicate as a SELECT returned
///    `[]` in 4ms, while dropping `kind` returned the row in 758ms. Addressing the row by primary
///    key cannot hit that planner path at all.
/// 2. **Cost.** A `WHERE`-scan over an unindexed field is ~0.75s per call; the planning backfill
///    issues ~37,000 of them, i.e. roughly 8 hours of pure table scanning. A primary-key write is
///    milliseconds.
pub(crate) async fn update_node_event_by_record(
    session: &DbSession,
    record_id: &str,
    weight: &urban_integrity_schema::types::node::NodeWeight,
) -> Result<()> {
    let wj = serde_json::to_string(weight)?;
    // `type::record($tb, $key)` — the same two-argument form projector.rs uses. Both parts are
    // bound, never interpolated.
    let (tb, key) = record_id.split_once(':').ok_or_else(|| {
        anyhow::anyhow!("update_node_event_by_record: malformed record id {record_id}")
    })?;
    // SurrealDB quotes the key half whenever it is not a bare identifier, and a single
    // hyphen is enough to trigger it. Every Contracts Finder key looks like
    // `NodeAdded_CF_ocds-b5fd17-0010f7d4-…`, so every one arrives quoted. `type::record()`
    // binds the key as a value, not as syntax, so the quotes are not delimiters there —
    // they become part of the key being looked up, which then matches nothing. `backfill
    // ward` aborted on its first contract for exactly this reason.
    let key = strip_record_key_quoting(key);
    let mut res = session
        .query(
            "UPDATE type::record($tb, $key) SET payload.weight_json = $wj",
            &[("tb", json!(tb)), ("key", json!(key)), ("wj", json!(wj))],
        )
        .await?;
    let updated: Vec<serde_json::Value> = res.take(0)?;
    if updated.is_empty() {
        anyhow::bail!("update_node_event_by_record: no record found for id={record_id}");
    }
    Ok(())
}

// `update_node_event(db, event_id, weight)` used to live here. It addressed rows with
// `WHERE event_id = $eid AND kind = 'NodeAdded'` — the planner-bug shape — so on a large
// indexed table it could match nothing and abort the backfill mid-run. Both callers
// (`ch.rs`, `ward.rs`) now carry the SurrealDB record id from their scan and write through
// [`update_node_event_by_record`], which cannot hit that path and is ~100× faster. Removed
// 2026-07-28 rather than left in place, so it cannot be reached for again.

/// Insert a brand-new NodeAdded record (Ward nodes that don't exist yet).
/// Insert (or overwrite) a `NodeUpdated` event carrying a corrected node weight.
///
/// Addressed by a **deterministic** record id — `NodeUpdated_{job}_{entity_id}` — so a re-run
/// overwrites its own previous row instead of accumulating a new one per pass. That is the same
/// defect Open Item 6d found in alert persistence, where a non-deterministic id produced 17,549
/// rows for 1,374 distinct clusters.
///
/// The write is verified to return a row. SurrealDB reports `status: OK` with an empty result for
/// an UPSERT that matched nothing, and trusting the status alone has already reported phantom
/// writes on this project twice — the ICIJ adapter's 814k and PPD's 2.8m, both of which wrote zero.
pub(crate) async fn upsert_node_updated_event(
    session: &DbSession,
    job: &str,
    entity_id: &str,
    weight: &urban_integrity_schema::types::node::NodeWeight,
) -> Result<()> {
    let key = format!("NodeUpdated_{job}_{entity_id}");
    let record = serde_json::json!({
        "event_id": Uuid::new_v4().to_string(),
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        "kind": "NodeUpdated",
        "payload": {
            "entity_id": entity_id,
            "weight_json": serde_json::to_string(weight)?,
        }
    });
    let written: Vec<serde_json::Value> = session
        .query(
            "UPSERT type::record($tb, $key) CONTENT $r",
            &[
                ("tb", json!(GRAPH_EVENTS_TABLE)),
                ("key", json!(key)),
                ("r", record),
            ],
        )
        .await?
        .take(0)?;
    if written.is_empty() {
        anyhow::bail!("upsert_node_updated_event: wrote no row for {entity_id}");
    }
    Ok(())
}

pub(crate) async fn insert_node_event(
    session: &DbSession,
    entity_id: &str,
    weight: &urban_integrity_schema::types::node::NodeWeight,
) -> Result<()> {
    let record = serde_json::json!({
        "event_id": Uuid::new_v4().to_string(),
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        "kind": "NodeAdded",
        "payload": {
            "entity_id": entity_id,
            "weight_json": serde_json::to_string(weight)?,
        }
    });
    let _: Vec<serde_json::Value> = session
        .query("CREATE graph_events CONTENT $r", &[("r", record)])
        .await?
        .take(0)?;
    Ok(())
}

/// Extract a string entity/edge ID from a SurrealDB RecordId value that may arrive
/// as a plain string or as a `{"tb": "...", "id": ...}` object.
pub(crate) fn extract_id_str(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    if let (Some(tb), Some(id_val)) = (v["tb"].as_str(), Some(&v["id"])) {
        let id_str = if let Some(s) = id_val.as_str() {
            s.to_owned()
        } else if let Some(s) = id_val["String"].as_str() {
            s.to_owned()
        } else {
            id_val.to_string().trim_matches('"').to_owned()
        };
        return Some(format!("{tb}:{id_str}"));
    }
    None
}

/// Insert (or overwrite) an `EdgeAdded` record under a **deterministic** id.
///
/// [`insert_edge_event`] mints a fresh `event_id` per call, so re-running a backfill duplicates
/// every edge it already wrote. Any job that can legitimately be re-run — because a bug was fixed
/// and the pass must be repeated — needs this instead, or the fix costs a duplicate edge set.
pub(crate) async fn upsert_edge_event(
    session: &DbSession,
    job: &str,
    src_id: &str,
    dst_id: &str,
    weight: &urban_integrity_schema::types::edge::EdgeWeight,
) -> Result<()> {
    let key = format!("EdgeAdded_{job}_{src_id}_{dst_id}");
    let record = serde_json::json!({
        "event_id": Uuid::new_v4().to_string(),
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        "kind": "EdgeAdded",
        "payload": {
            "src_id": src_id,
            "dst_id": dst_id,
            "weight_json": serde_json::to_string(weight)?,
        }
    });
    let written: Vec<serde_json::Value> = session
        .query(
            "UPSERT type::record($tb, $key) CONTENT $r",
            &[
                ("tb", json!(GRAPH_EVENTS_TABLE)),
                ("key", json!(key)),
                ("r", record),
            ],
        )
        .await?
        .take(0)?;
    if written.is_empty() {
        anyhow::bail!("upsert_edge_event: wrote no row for {src_id} -> {dst_id}");
    }
    Ok(())
}

/// Insert a brand-new EdgeAdded record (Represents edges that don't exist yet).
pub(crate) async fn insert_edge_event(
    session: &DbSession,
    src_id: &str,
    dst_id: &str,
    weight: &urban_integrity_schema::types::edge::EdgeWeight,
) -> Result<()> {
    let record = serde_json::json!({
        "event_id": Uuid::new_v4().to_string(),
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        "kind": "EdgeAdded",
        "payload": {
            "src_id": src_id,
            "dst_id": dst_id,
            "weight_json": serde_json::to_string(weight)?,
        }
    });
    let _: Vec<serde_json::Value> = session
        .query("CREATE graph_events CONTENT $r", &[("r", record)])
        .await?
        .take(0)?;
    Ok(())
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use surrealdb::engine::any::Any;
    use surrealdb::Surreal;

    // These tests only mean anything against a real SurrealDB: the defect they pin —
    // an indexed `kind` equality paired with a `CONTAINS` scan returning a truncated,
    // non-deterministic row set — does not exist in any in-memory fake. CI runs them
    // (`URBAN_DB_TESTS=1`, per .github/workflows/ci.yml).
    fn db_tests_enabled() -> bool {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return false;
        }
        std::net::TcpStream::connect("[::1]:8000").is_ok()
            || std::net::TcpStream::connect("127.0.0.1:8000").is_ok()
    }

    /// Returns the session plus the namespace guard; callers must bind the
    /// guard so the namespace outlives the test.
    async fn connect_scratch_db(
        suffix: &str,
    ) -> (DbSession, urban_integrity_test_support::NsGuard) {
        let guard = urban_integrity_test_support::NsGuard::with_prefix("test_backfill_scan");
        use surrealdb::opt::auth::Root;
        let db: Surreal<Any> = Surreal::init();
        db.connect("http://localhost:8000").await.expect("connect");
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await
        .expect("signin");
        use_ns_db_with_retry(&db, guard.name(), &format!("scan_{suffix}")).await;
        db.query("REMOVE TABLE graph_events").await.ok();
        // The index is the whole point: without it the planner cannot take the bad path,
        // so a test on an unindexed table would pass against the buggy query shape too.
        db.query("DEFINE TABLE graph_events SCHEMALESS")
            .await
            .expect("define table");
        db.query("DEFINE INDEX idx_graph_events_kind ON TABLE graph_events COLUMNS kind")
            .await
            .expect("define index");
        // Root credentials are real here, so the scratch session exercises the same re-auth-
        // capable type the backfills use rather than a stripped-down stand-in.
        let session = DbSession::new(
            std::sync::Arc::new(db),
            SessionCreds {
                username: "root".to_string(),
                password: "root".to_string(),
                namespace: guard.name().to_string(),
                database: format!("scan_{suffix}"),
            },
        );
        (session, guard)
    }

    async fn seed(session: &DbSession, kind: &str, marker: &str, n: usize) {
        let db = session.handle();
        for i in 0..n {
            let record = serde_json::json!({
                "event_id": format!("{kind}-{marker}-{i}"),
                "timestamp": i as u64,
                "kind": kind,
                "payload": {
                    "entity_id": format!("seed:{i}"),
                    "weight_json": format!("{{\"type\":\"{marker}\",\"name\":\"n{i}\"}}"),
                }
            });
            let _: Vec<serde_json::Value> = db
                .query("CREATE graph_events CONTENT $r")
                .bind(("r", record))
                .await
                .expect("seed insert")
                .take(0)
                .expect("seed take");
        }
    }

    /// The scan must return every matching row and nothing else, across page boundaries.
    ///
    /// 50 rows at page size 7 forces 8 pages, so an off-by-one in the `START` advance — the
    /// specific way the old code lost rows — drops or duplicates rows rather than passing.
    #[tokio::test]
    async fn db_scan_events_by_type_paginates_and_filters_kind() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = connect_scratch_db("paginate").await;
        seed(&db, "NodeAdded", "Widget", 50).await;
        // Same marker, different kind — must be filtered out.
        seed(&db, "EdgeAdded", "Widget", 20).await;
        // Different marker, same kind — must not be picked up.
        seed(&db, "NodeAdded", "Gadget", 15).await;

        let rows = scan_events_by_type_paged(&db, "NodeAdded", "\"type\":\"Widget\"", 7)
            .await
            .expect("scan");

        assert_eq!(
            rows.len(),
            50,
            "scan must return all 50 NodeAdded Widget rows across 8 pages, not a truncated set"
        );
        assert!(
            rows.iter().all(|r| r["kind"].as_str() == Some("NodeAdded")),
            "scan must filter out EdgeAdded rows carrying the same type marker"
        );
        let ids: std::collections::HashSet<&str> =
            rows.iter().filter_map(|r| r["event_id"].as_str()).collect();
        assert_eq!(ids.len(), 50, "paging must not duplicate rows across pages");
    }

    /// The scan result must agree with `count()` on the same predicate.
    ///
    /// This is the invariant the old query shape violated in production: `count()` reported
    /// 33,046 while the row-returning form handed back 6,033, and nothing failed.
    #[tokio::test]
    async fn db_scan_agrees_with_count_where_the_old_shape_did_not() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = connect_scratch_db("count_parity").await;
        seed(&db, "NodeAdded", "Entity", 120).await;

        let counted: Vec<serde_json::Value> = db
            .handle()
            .query(
                "SELECT count() AS n FROM graph_events \
                 WHERE kind = 'NodeAdded' \
                   AND payload.weight_json CONTAINS '\"type\":\"Entity\"' GROUP ALL",
            )
            .await
            .expect("count query")
            .take(0)
            .expect("count take");
        let expected = counted
            .first()
            .and_then(|r| r["n"].as_u64())
            .expect("count result") as usize;

        let rows = scan_events_by_type_paged(&db, "NodeAdded", "\"type\":\"Entity\"", 25)
            .await
            .expect("scan");

        assert_eq!(
            rows.len(),
            expected,
            "scan must return exactly as many rows as count() reports for the same predicate"
        );
    }

    /// `cursor::resume_index`'s fallback compares record ids as Rust strings, so the scan's
    /// `ORDER BY id` has to agree with Rust's string ordering.
    ///
    /// It does — SurrealDB orders string record ids lexicographically — but the resume path
    /// depends on it silently, and a SurrealDB upgrade that changed record-id ordering would
    /// make a resumed run skip or repeat a slice of its candidate list with nothing failing.
    /// Pin it here so the upgrade breaks a test instead.
    #[tokio::test]
    async fn db_scan_returns_ids_in_rust_string_order() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (db, _ns) = connect_scratch_db("id_order").await;
        seed(&db, "NodeAdded", "Ordered", 40).await;

        let rows = scan_events_by_type_paged(&db, "NodeAdded", "\"type\":\"Ordered\"", 9)
            .await
            .expect("scan");

        let ids: Vec<String> = rows
            .iter()
            .filter_map(|r| extract_id_str(&r["id"]))
            .collect();
        assert_eq!(ids.len(), 40, "every scanned row must carry a record id");
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            ids, sorted,
            "scan order must match Rust string order — cursor::resume_index relies on it"
        );
    }
}
