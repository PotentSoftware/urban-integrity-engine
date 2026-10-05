//! LOCKED SYNTAX FOR D-05 — 28-06 must copy this exactly, not re-derive it.
//!
//! This is a throwaway spike, proven against an isolated `TestNs`, never against the live
//! `an_anglesey_aebaa785` analysis database. Its only purpose is to lock down the exact
//! `DELETE ... RETURN BEFORE` SurrealQL shape (and the exact Rust bind/type shapes) that
//! returns a *real removed-count* — i.e. the number of rows SurrealDB actually deleted, not
//! the number of keys the caller sent — through the `surrealdb = "=3.1.0-beta.2"` Rust SDK's
//! `.take(0)`.
//!
//! # The locked form (verified against `TestNs`, 2026-08-22)
//!
//! ```surql
//! DELETE type::table($table) WHERE id IN $ids RETURN BEFORE
//! ```
//!
//! bound as:
//!
//! ```rust,ignore
//! let ids: Vec<surrealdb::types::RecordId> = keys
//!     .iter()
//!     .map(|k| surrealdb::types::RecordId::new(table, k.clone()))
//!     .collect();
//! let removed: Vec<serde_json::Value> = db
//!     .query("DELETE type::table($table) WHERE id IN $ids RETURN BEFORE")
//!     .bind(("table", table.to_string()))
//!     .bind(("ids", ids))
//!     .await?
//!     .take(0)?;
//! let removed_count = removed.len(); // the REAL removed-count — this is D-05.
//! ```
//!
//! `removed.len()` is the real number of records SurrealDB actually removed: deleting 2 of 3
//! seeded rows returns an array of exactly 2 elements, and a follow-up `SELECT count() FROM
//! spike_probe` reports the 1 row left behind. This is proof the RETURN BEFORE array element
//! count is real evidence, not a re-statement of what the caller sent (the research notes' Open
//! Question 2 / D-05's precondition).
//!
//! `surrealdb::types::RecordId` (re-exported from `surrealdb_types` via `surrealdb::types`)
//! implements `SurrealValue` via its own `#[derive]`, so `Vec<RecordId>` binds directly with no
//! `type::record()` string construction needed inside the `WHERE` clause.
//!
//! The documented `FOR`-loop-with-accumulator fallback (used only if the set-based form above
//! had failed to parse or bind) is:
//!
//! ```surql
//! FOR $k IN $keys {
//!     LET $r = (DELETE type::record($table, $k) RETURN BEFORE);
//!     IF $r != NONE { LET $removed = array::add($removed, $r); };
//! };
//! RETURN $removed;
//! ```
//!
//! It was not needed — the set-based form above worked as expected on the first attempt.

use urban_integrity_test_support::TestNs;

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Seeds three throwaway `spike_probe` records, deletes two of them via the locked
/// `DELETE ... WHERE id IN $ids RETURN BEFORE` form, and asserts the returned array length is
/// the REAL removed-count (2), with the third row still present afterwards.
#[tokio::test]
async fn delete_return_before_yields_real_removed_count() {
    if !db_tests_enabled() {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return;
    }

    // Never the live analysis DB — a throwaway TestNs, mandatory per docs/TESTING.md.
    let db = TestNs::connect_with_prefix("test_delspike", "main").await;

    // Seed three throwaway records into a scratch table.
    db.query("CREATE spike_probe:one CONTENT {}")
        .await
        .expect("seed spike_probe:one")
        .check()
        .expect("seed spike_probe:one returned a statement error");
    db.query("CREATE spike_probe:two CONTENT {}")
        .await
        .expect("seed spike_probe:two")
        .check()
        .expect("seed spike_probe:two returned a statement error");
    db.query("CREATE spike_probe:three CONTENT {}")
        .await
        .expect("seed spike_probe:three")
        .check()
        .expect("seed spike_probe:three returned a statement error");

    // Delete two of the three via the locked set-based form.
    let table = "spike_probe";
    let ids: Vec<surrealdb::types::RecordId> = ["one", "two"]
        .iter()
        .map(|k| surrealdb::types::RecordId::new(table, *k))
        .collect();

    let removed: Vec<serde_json::Value> = db
        .query("DELETE type::table($table) WHERE id IN $ids RETURN BEFORE")
        .bind(("table", table.to_string()))
        .bind(("ids", ids))
        .await
        .expect("DELETE ... RETURN BEFORE failed")
        .take(0)
        .expect("failed to decode RETURN BEFORE result");

    assert_eq!(
        removed.len(),
        2,
        "RETURN BEFORE must report the REAL removed-count (2), not keys sent; got {removed:?}"
    );

    // Cross-check: exactly one row (`spike_probe:three`) remains. `GROUP ALL` yields
    // `{ "count": N }`, which the SDK won't coerce straight into an integer — take into a
    // JSON value and read the number out (mirrors `count_graph_events` in
    // crates/core/src/persistence/surreal.rs).
    let count_rows: Vec<serde_json::Value> = db
        .query("SELECT count() FROM spike_probe GROUP ALL")
        .await
        .expect("count query failed")
        .take(0)
        .expect("failed to decode count() result");
    let remaining = count_rows.first().and_then(|r| r["count"].as_i64());
    assert_eq!(
        remaining,
        Some(1),
        "exactly 1 spike_probe row should remain after deleting 2 of 3"
    );

    // Clean up spike_probe explicitly (readability only — TestNs/NsGuard's Drop removes the
    // whole namespace regardless, so this is not load-bearing for isolation).
    db.query("DELETE spike_probe")
        .await
        .expect("cleanup delete failed")
        .check()
        .expect("cleanup delete returned a statement error");
}
