use anyhow::Context as _;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Receiver;
use urban_integrity_core::graph::types::GraphMutation;

/// Build a deterministic WAL record ID: `<event_type>_<sanitised_src>_<sanitised_dst>`.
///
/// Sanitisation replaces all chars outside [a-zA-Z0-9_-] with `_`.
/// This ensures readable, injection-safe record IDs in SurrealDB (D-06, D-07).
pub fn build_wal_record_id(event_type: &str, src: &str, dst: &str) -> String {
    let sanitise = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("{}_{}_{}", event_type, sanitise(src), sanitise(dst))
}

/// Write a single WAL event to `graph_events` using a deterministic record ID.
///
/// If the record ID already exists (immutable WAL, D-07), returns Ok(()) — skip.
/// All other errors propagate immediately (fail-fast, D-04).
///
/// The SurrealQL query uses only bound parameters — never string interpolation (T-21-01-02).
pub async fn write_wal_event(
    db: &Surreal<Any>,
    event_type: &str,
    src_id: &str,
    dst_id: &str,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    let record_id = build_wal_record_id(event_type, src_id, dst_id);

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let r = serde_json::json!({
        "event_id": record_id,
        "timestamp": timestamp_ms,
        "kind": event_type,
        "payload": payload,
    });

    // T-24-07 / CR: `.and_then(|mut r| r.take(0))` is load-bearing, not decoration.
    //
    // `query(...).await` resolves to `Ok(Response)` whenever the *transport* succeeded.
    // Statement-level failures — a schema rejection, the SurrealDB 3.x datetime-coercion
    // drop — live INSIDE that `Response` and surface only when it is consumed via
    // `.check()` or `.take()`. Dropping the `Response` therefore turned every such failure
    // into `Ok(())`: the drain reported success having written nothing.
    //
    // That is not hypothetical. It is the exact shape of the two most expensive incidents
    // in this project's history, both on this same `CREATE ... CONTENT` construction: the
    // ICIJ adapter wrote 0 rows for its entire operational life while logging ~814k
    // written, and the PPD load reported 2,795,151 rows against an empty staging table.
    // Every other write path in this crate already consumes its Response; this one did not,
    // which is what made the fail-fast `?` chain in `drain_to_wal` a net that only caught
    // connection errors.
    let result = db
        .query("CREATE type::record('graph_events', $id) CONTENT $r")
        .bind(("id", record_id.clone()))
        .bind(("r", r))
        .await
        .and_then(|mut response| response.take::<Vec<serde_json::Value>>(0));

    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") || msg.contains("AlreadyExists") {
                // D-07: WAL is immutable — skip if event already recorded
                Ok(())
            } else {
                Err(e).context(format!("graph_events insert failed for id={record_id}"))
            }
        }
    }
}

/// Drain `GraphMutation` events from the channel and write each to `graph_events`.
///
/// Returns `Ok(())` when the sender is dropped (channel closed).
/// Returns `Err` on the first WAL write failure (fail-fast, D-04).
///
/// # ENR-11 Gate
/// Adapter self-gating ensures only auto_linked/confirmed mutations reach this channel.
/// See `confidence.rs` `AUTO_LINK_THRESHOLD`. The drain task writes everything it receives
/// and relies on adapter self-gating — adapters (PSC, Disqualified, EC) only emit `AddEdge`
/// for confirmed matches.
///
/// `RemoveNode` and `UpdateNode` mutations are silently skipped (no WAL write).
pub async fn drain_to_wal(
    mut rx: Receiver<GraphMutation>,
    db: std::sync::Arc<Surreal<Any>>,
) -> anyhow::Result<()> {
    while let Some(mutation) = rx.recv().await {
        match mutation {
            // Enricher adapters emit `AddNode`; `UpsertNode` is handled identically so
            // this match stays exhaustive (the ingestion pipeline's `UpsertNode` does not
            // flow through this enricher WAL drain, but the compiler requires the arm).
            GraphMutation::AddNode { ref id, ref weight }
            | GraphMutation::UpsertNode { ref id, ref weight } => {
                let payload = serde_json::json!({
                    "entity_id": id.0,
                    "weight_json": serde_json::to_string(weight).unwrap_or_default(),
                });
                write_wal_event(&db, "NodeAdded", &id.0, &id.0, payload).await?;
                // fail-fast: ? propagates on first error (D-04)
            }
            GraphMutation::AddEdge {
                ref src,
                ref dst,
                ref weight,
            } => {
                let payload = serde_json::json!({
                    "src_id": src.0,
                    "dst_id": dst.0,
                    "weight_json": serde_json::to_string(weight).unwrap_or_default(),
                });
                write_wal_event(&db, "EdgeAdded", &src.0, &dst.0, payload).await?;
            }
            // ENR-11: RemoveNode and UpdateNode do not produce WAL events —
            // the WAL records only creation events; removals/updates are handled
            // by the hot-graph coordinator on the serve path.
            GraphMutation::RemoveNode { .. } | GraphMutation::UpdateNode { .. } => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_wal_record_id_sanitises_special_chars() {
        let id = build_wal_record_id("NodeAdded", "entity:GB-COH:123", "psc:x");
        // ':' is outside [a-zA-Z0-9_-] so should become '_'
        assert_eq!(id, "NodeAdded_entity_GB-COH_123_psc_x");
    }

    #[test]
    fn test_build_wal_record_id_deterministic() {
        let id1 = build_wal_record_id("EdgeAdded", "entity:abc", "psc:def");
        let id2 = build_wal_record_id("EdgeAdded", "entity:abc", "psc:def");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_build_wal_record_id_empty_components() {
        // Should not panic on empty strings
        let id = build_wal_record_id("NodeAdded", "", "");
        assert_eq!(id, "NodeAdded__");
    }

    // ── DB-gated: statement-level failure detection (T-24-07) ────────────────
    //
    // The pure tests above cannot see this class of defect at all. `write_wal_event`
    // returned `Ok(())` for any failure that lives inside the `Response` rather than in
    // the transport — schema rejections, the SurrealDB 3.x datetime-coercion drop — which
    // is precisely how the ICIJ adapter logged ~814k rows written while writing zero.
    // Only a real database can prove the difference.

    fn db_tests_enabled() -> bool {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return false;
        }
        std::net::TcpStream::connect("[::1]:8000").is_ok()
            || std::net::TcpStream::connect("127.0.0.1:8000").is_ok()
    }

    /// Returns the handle plus the namespace guard; callers must bind the
    /// guard so the namespace outlives the test.
    async fn connect_scratch_db(
        suffix: &str,
    ) -> (Surreal<Any>, urban_integrity_test_support::NsGuard) {
        let guard = urban_integrity_test_support::NsGuard::with_prefix("test_wal_writer");
        use surrealdb::opt::auth::Root;
        let db: Surreal<Any> = Surreal::init();
        db.connect("http://localhost:8000").await.expect("connect");
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await
        .expect("signin");
        db.use_ns(guard.name())
            .use_db(format!("failloud_{suffix}"))
            .await
            .expect("use ns/db");
        (db, guard)
    }

    /// A row the schema rejects must produce `Err`, not a silent success.
    ///
    /// The rejection is induced the same way SurrealDB 3.x drops real rows: a strict field
    /// type the payload cannot be coerced into. `write_wal_event` sends `timestamp` as an
    /// integer, so declaring it `datetime` on a SCHEMAFULL table makes the CREATE fail at
    /// statement level while the transport succeeds — exactly the case the old code mapped
    /// to `Ok(())`.
    #[tokio::test]
    async fn db_write_wal_event_errors_when_the_row_is_rejected() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: db_write_wal_event_errors_when_the_row_is_rejected (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let (db, _ns) = connect_scratch_db("reject").await;
        db.query("REMOVE TABLE graph_events").await.ok();
        db.query(
            "DEFINE TABLE graph_events SCHEMAFULL; \
             DEFINE FIELD event_id ON graph_events TYPE string; \
             DEFINE FIELD timestamp ON graph_events TYPE datetime; \
             DEFINE FIELD kind ON graph_events TYPE string; \
             DEFINE FIELD payload ON graph_events TYPE object FLEXIBLE;",
        )
        .await
        .expect("define strict schema");

        let result = write_wal_event(
            &db,
            "NodeAdded",
            "entity:GB-COH:99999999",
            "entity:GB-COH:99999999",
            serde_json::json!({ "entity_id": "entity:GB-COH:99999999" }),
        )
        .await;

        assert!(
            result.is_err(),
            "a schema-rejected row reported success — this is the ICIJ failure mode, where \
             the drain claims to have written rows that do not exist"
        );

        let remaining: Vec<serde_json::Value> = db
            .query("SELECT * FROM graph_events")
            .await
            .expect("query")
            .take(0)
            .expect("take");
        assert!(
            remaining.is_empty(),
            "nothing should have been written, so the error must not be spurious"
        );
    }

    /// The happy path must still succeed, and the D-07 immutability skip must survive:
    /// re-writing the same deterministic record id is a no-op, not an error.
    #[tokio::test]
    async fn db_write_wal_event_succeeds_and_stays_idempotent() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: db_write_wal_event_succeeds_and_stays_idempotent (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let (db, _ns) = connect_scratch_db("ok").await;
        db.query("REMOVE TABLE graph_events").await.ok();

        let write = || {
            write_wal_event(
                &db,
                "NodeAdded",
                "entity:GB-COH:11111111",
                "entity:GB-COH:11111111",
                serde_json::json!({ "entity_id": "entity:GB-COH:11111111" }),
            )
        };

        write().await.expect("first write should succeed");
        write()
            .await
            .expect("re-writing the same record id is the D-07 immutability skip, not an error");

        let rows: Vec<serde_json::Value> = db
            .query("SELECT * FROM graph_events")
            .await
            .expect("query")
            .take(0)
            .expect("take");
        assert_eq!(
            rows.len(),
            1,
            "the WAL is append-only and deduped by record id"
        );
    }
}
