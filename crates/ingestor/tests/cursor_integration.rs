//! SurrealDB-backed cursor store integration tests.
//!
//! Requires: URBAN_DB_TESTS=1 and running SurrealDB on localhost:8000 (root/root).

use urban_integrity_ingestor::ch::{client::CursorStore, surreal_cursor::SurrealCursorStore};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Returns the store plus the namespace guard — the caller must hold the
/// guard for the lifetime of the test, or the namespace is removed early.
async fn make_cursor() -> (SurrealCursorStore, urban_integrity_test_support::TestNs) {
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_cursor", "main").await;
    (SurrealCursorStore::new(db.arc()), db)
}

// ── Load ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cursor_load_on_empty_db_returns_none() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    assert_eq!(
        cursor.load_timepoint().await,
        None,
        "fresh DB should have no cursor"
    );
}

// ── Persist + load round-trip ─────────────────────────────────────────────────

#[tokio::test]
async fn test_cursor_persist_and_load_round_trip() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    cursor.persist_timepoint(12_345_678).await;
    let loaded = cursor.load_timepoint().await;
    assert_eq!(loaded, Some(12_345_678));
}

#[tokio::test]
async fn test_cursor_persist_zero_timepoint() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    cursor.persist_timepoint(0).await;
    assert_eq!(cursor.load_timepoint().await, Some(0));
}

#[tokio::test]
async fn test_cursor_persist_large_timepoint() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    cursor.persist_timepoint(i64::MAX).await;
    assert_eq!(cursor.load_timepoint().await, Some(i64::MAX));
}

// ── Overwrite (UPSERT semantics) ───────────────────────────────────────────────

#[tokio::test]
async fn test_cursor_overwrite_replaces_previous_value() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    cursor.persist_timepoint(100).await;
    cursor.persist_timepoint(200).await;
    cursor.persist_timepoint(300).await;
    assert_eq!(
        cursor.load_timepoint().await,
        Some(300),
        "last write should win"
    );
}

// ── Clear ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cursor_clear_removes_timepoint() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    cursor.persist_timepoint(99_999).await;
    assert_eq!(cursor.load_timepoint().await, Some(99_999));
    cursor.clear_timepoint().await;
    assert_eq!(
        cursor.load_timepoint().await,
        None,
        "cleared cursor should return None"
    );
}

#[tokio::test]
async fn test_cursor_clear_on_empty_db_is_non_fatal() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    // Clearing when nothing is stored must not panic or error.
    cursor.clear_timepoint().await;
    assert_eq!(cursor.load_timepoint().await, None);
}

#[tokio::test]
async fn test_cursor_re_persist_after_clear() {
    if !db_tests_enabled() {
        return;
    }
    let (cursor, _ns) = make_cursor().await;
    cursor.persist_timepoint(111).await;
    cursor.clear_timepoint().await;
    cursor.persist_timepoint(222).await;
    assert_eq!(
        cursor.load_timepoint().await,
        Some(222),
        "should work after clear/re-persist cycle"
    );
}
