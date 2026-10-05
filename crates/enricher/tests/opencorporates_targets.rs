//! DB-gated regression test for the OpenCorporates target-selection query.
//! Requires a live SurrealDB (v3, port 8000, root/root) and `URBAN_DB_TESTS=1`;
//! skips cleanly otherwise.
//!
//! The adapter selected its candidates with `WHERE company_number IS NOT NULL`. SurrealDB
//! distinguishes an ABSENT field (NONE) from one explicitly set to null (NULL), and the
//! projector simply omits the key for a company that has no number — so the predicate excluded
//! nothing. Measured live on `an_anglesey_aebaa785`, 2026-08-16: `IS NOT NULL` returned all
//! 72,123 entity rows, `IS NOT NONE` returned the 14,720 that carry a number. The adapter's
//! Rust `as_str()?` then dropped the other 57,403 without a word, so the clause read as a
//! filter, cost a full-table scan, and filtered nothing.
//!
//! The API half of this adapter cannot be exercised here: it needs an OpenCorporates key and
//! an ODbL acknowledgement, neither of which is configured. This test pins the half that runs
//! entirely against the database.

use urban_integrity_test_support::TestNs;

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Seed an `entity` row exactly as the projector writes one: record id == entity_id, and the
/// `company_number` key ABSENT (not null) when the company has no number.
async fn seed_entity(db: &TestNs, id: &str, name: &str, company_number: Option<&str>) {
    let sql = match company_number {
        Some(_) => {
            "CREATE type::record('entity', $id) CONTENT \
                    { entity_id: $id, name: $name, company_number: $cn }"
        }
        None => "CREATE type::record('entity', $id) CONTENT { entity_id: $id, name: $name }",
    };
    db.query(sql)
        .bind(("id", id.to_owned()))
        .bind(("name", name.to_owned()))
        .bind(("cn", company_number.map(str::to_owned)))
        .await
        .expect("seed failed")
        .check()
        .expect("seed rejected by schema");
}

#[tokio::test]
async fn test_target_query_excludes_entities_without_a_company_number() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_target_query_excludes_entities_without_a_company_number");
        return;
    }
    let db = TestNs::connect("main").await;

    seed_entity(&db, "GB-COH:00133804", "Arco Limited", Some("00133804")).await;
    seed_entity(&db, "GB-COH:00199841", "Bucher Municipal", Some("00199841")).await;
    // The shape the old predicate let through: no company_number key at all.
    seed_entity(&db, "director:jane-doe", "Jane Doe", None).await;
    seed_entity(&db, "lpa:anglesey", "Isle of Anglesey County Council", None).await;

    let rows: Vec<serde_json::Value> = db
        .query(
            "SELECT entity_id, name, company_number FROM entity \
             WHERE company_number IS NOT NONE",
        )
        .await
        .expect("target query failed")
        .take(0)
        .expect("target query returned no result set");

    let mut ids: Vec<String> = rows
        .iter()
        .map(|r| r["entity_id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();

    assert_eq!(
        ids,
        vec!["GB-COH:00133804".to_owned(), "GB-COH:00199841".to_owned()],
        "the target query must exclude entities with no company_number — `IS NOT NULL` \
         returned all four"
    );
}

/// The predicate the adapter used to carry, pinned as the counter-example so a future edit
/// cannot quietly restore it believing it equivalent.
#[tokio::test]
async fn test_is_not_null_does_not_filter_absent_fields() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_is_not_null_does_not_filter_absent_fields");
        return;
    }
    let db = TestNs::connect("main").await;

    seed_entity(&db, "GB-COH:00133804", "Arco Limited", Some("00133804")).await;
    seed_entity(&db, "director:jane-doe", "Jane Doe", None).await;

    let rows: Vec<serde_json::Value> = db
        .query("SELECT entity_id FROM entity WHERE company_number IS NOT NULL")
        .await
        .expect("query failed")
        .take(0)
        .expect("no result set");

    assert_eq!(
        rows.len(),
        2,
        "IS NOT NULL is not a filter for absent fields in SurrealDB 3.x — if this ever \
         returns 1, the engine's NONE/NULL semantics changed and the adapter's predicate \
         should be revisited"
    );
}
