//! PIV-05: a saved investigation survives being closed, and its clocks are real.
//!
//! # What this file is actually testing
//!
//! Not "the write returned `Ok`". This project has twice shipped a write path that reported
//! success over an empty table:
//!
//! * the **PPD enrichment** logged "2795151 written" against **0 staged rows**, because SurrealDB
//!   3.x will not coerce a string into a `datetime` field and a `CONTENT` write with a bare
//!   `.await?` drops the row silently. Every unit test was green; it was caught at a live verify
//!   gate;
//! * the **ICIJ adapter** wrote 0 rows for its entire life behind `let _ = db.query(...)` while
//!   logging 814k writes.
//!
//! A round-trip test that asserted `is_ok()` would have passed happily through both. So every test
//! below reads the row back and checks what is in it, and
//! [`db_the_stored_timestamps_are_datetimes_and_not_strings`] asks the **database** what type the
//! column holds — `type::is_datetime` — because the JSON that comes back over the wire renders a
//! real `datetime` and an RFC3339 string identically, and a reader in Rust cannot tell them apart.
//!
//! # "Reopen" means a new connection, not a restarted server
//!
//! [`db_an_investigation_reopens_through_a_fresh_connection`] drops every handle it wrote through
//! and opens new ones. It deliberately does **not** restart SurrealDB: the instance on this box
//! serves the live analysis store, and a `scripts/surreal-start.sh` from the wrong directory
//! silently creates an empty one (see docs/DEVELOPMENT.md). What the requirement needs is that nothing lives
//! in process memory or in a browser — a second connection reading rows a first one wrote is that
//! claim.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test saved_investigations
//! ```
//!
//! Every test here is `db_`-prefixed and gated on `URBAN_DB_TESTS=1`, skipping by returning — so a
//! gated run reports the same passing count as an ungated one, and duration is the only witness a
//! database was reached. Namespaces come from `urban-integrity-test-support`, which drops them.

use serde_json::Value;
use surrealdb::engine::any::Any;
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;

use urban_integrity_core::persistence::saved_investigations::{
    create_investigation, delete_investigation, get_investigation, list_investigations, list_pins,
    pin_member, rename_investigation, set_investigation_notes, set_pin_note, unpin_member,
    PinOrigin, PinTarget, PIN_TABLE, SAVED_INVESTIGATION_SCHEMA_VERSION,
};
use urban_integrity_test_support::{credentials, db_tests_enabled, endpoint, NsGuard, TestNs};

/// A second, independent client on the same namespace and database.
///
/// This is what "reopen" means here: a connection that shares nothing with the one that wrote.
async fn fresh_connection(ns: &str, db_name: &str) -> Surreal<Any> {
    let (username, password) = credentials();
    let db = surrealdb::engine::any::connect(endpoint())
        .await
        .expect("SurrealDB unreachable");
    db.signin(Root { username, password })
        .await
        .expect("SurrealDB root sign-in failed");
    db.use_ns(ns)
        .use_db(db_name)
        .await
        .expect("namespace/database selection failed");
    db.query(urban_integrity_schema::db::SCHEMA_DEF)
        .await
        .expect("schema init failed")
        .check()
        .expect("schema init returned a statement error");
    db
}

fn entity(id: &str) -> PinTarget {
    PinTarget::Entity {
        entity_id: id.to_string(),
    }
}

fn edge(from: &str, to: &str, relation: &str) -> PinTarget {
    PinTarget::Edge {
        from: from.to_string(),
        to: to.to_string(),
        relation: relation.to_string(),
    }
}

/// Count rows the long way round, so a comparison is never made over an empty table.
async fn count(db: &Surreal<Any>, table: &str) -> usize {
    let rows: Vec<Value> = db
        .query(format!("SELECT count() AS n FROM {table} GROUP ALL"))
        .await
        .expect("count query")
        .check()
        .expect("count statement")
        .take(0)
        .expect("count decode");
    rows.first()
        .and_then(|r| r.get("n"))
        .and_then(Value::as_i64)
        .unwrap_or(0) as usize
}

// ── The headline: it comes back ──────────────────────────────────────────────

/// **The requirement.** A named investigation with notes and pinned members is written through one
/// connection and read back, whole, through another.
///
/// PROVE THIS CAN FAIL: in `create_investigation`, replace `.bind(("now", now))` with
/// `.bind(("now", now.to_rfc3339()))`. SurrealDB refuses the string for the `datetime` column and
/// `.check()?` surfaces it, so the test fails at the create — which is the PPD incident, caught.
#[tokio::test]
async fn db_an_investigation_reopens_through_a_fresh_connection() {
    if !db_tests_enabled() {
        return;
    }
    // `NsGuard` rather than `TestNs` here, and deliberately: `TestNs` removes the namespace when
    // the handle drops, and this test has to drop the writing handle while the data stays on disk.
    // The guard outlives both connections and cleans up at the end.
    let guard = NsGuard::new();
    let db = fresh_connection(guard.name(), "an_test_reopen").await;

    let investigation = create_investigation(&db, "Anglesey land assembly", "opening note")
        .await
        .expect("create_investigation");
    let id = investigation.id.clone();

    pin_member(
        &db,
        &id,
        &entity("GB-COH:12345678"),
        PinOrigin::Profile,
        Some("the applicant"),
    )
    .await
    .expect("pin the company");
    pin_member(
        &db,
        &id,
        &entity("director:jane-doe"),
        PinOrigin::Canvas,
        None,
    )
    .await
    .expect("pin the director");
    pin_member(
        &db,
        &id,
        &edge("GB-COH:12345678", "director:jane-doe", "directs"),
        PinOrigin::Subgraph,
        Some("visible on the canvas after filtering"),
    )
    .await
    .expect("pin the edge");

    // Non-vacuity: three members exist before anything is compared.
    assert_eq!(
        count(&db, PIN_TABLE).await,
        3,
        "three members must be on disk before a round trip can prove anything"
    );

    // Everything the writer held goes away. Nothing this investigation needs is in process
    // memory; the next line proves it by asking a connection that never saw the writes.
    drop(db);

    let reopened = fresh_connection(guard.name(), "an_test_reopen").await;

    let read_back = get_investigation(&reopened, &id)
        .await
        .expect("get_investigation")
        .expect("the investigation must still be there after the writer is gone");
    assert_eq!(read_back.name, "Anglesey land assembly");
    assert_eq!(read_back.notes, "opening note");
    assert_eq!(read_back.analysis_db, "an_test_reopen");
    assert_eq!(read_back.schema_version, SAVED_INVESTIGATION_SCHEMA_VERSION);
    assert_eq!(
        read_back.created_at, investigation.created_at,
        "the created_at that comes back must be the instant that was written, to the same \
         precision — a datetime that survived as text would compare unequal here"
    );

    let pins = list_pins(&reopened, &id).await.expect("list_pins");
    assert_eq!(pins.len(), 3, "every member must reopen");

    let company = pins
        .iter()
        .find(|p| p.target == entity("GB-COH:12345678"))
        .expect("the company must reopen");
    assert_eq!(
        company.note, "the applicant",
        "a per-member note is work product and must survive"
    );
    assert_eq!(company.origin, PinOrigin::Profile);

    let director = pins
        .iter()
        .find(|p| p.target == entity("director:jane-doe"))
        .expect("the director must reopen");
    assert_eq!(
        director.note, "",
        "a member pinned without a note reopens with an empty one"
    );
    assert_eq!(director.origin, PinOrigin::Canvas);

    let relation = pins
        .iter()
        .find(|p| p.target == edge("GB-COH:12345678", "director:jane-doe", "directs"))
        .expect("the EDGE must reopen — a node-only set cannot reproduce a filtered subgraph");
    assert_eq!(relation.origin, PinOrigin::Subgraph);

    // `guard` drops here and takes the namespace with it.
}

/// **The assertion the whole plan turns on.** The four timestamp columns hold `datetime` values,
/// as the database itself reports them.
///
/// Over the wire a real `datetime` and an RFC3339 string are the same JSON string, so nothing in
/// Rust can tell them apart — `type::is_datetime` is asked in SurrealQL, where the difference
/// exists.
///
/// PROVE THIS CAN FAIL: bind any of the four as `Utc::now().to_rfc3339()` instead of the chrono
/// value. The column refuses it and the write errors; loosen the column to `TYPE string` as well
/// and this test goes red on `is_dt`, which is the combination that silently cost 2.8m PPD rows.
#[tokio::test]
async fn db_the_stored_timestamps_are_datetimes_and_not_strings() {
    if !db_tests_enabled() {
        return;
    }
    let db = TestNs::connect("an_test_datetimes").await;

    let investigation = create_investigation(&db, "clock check", "")
        .await
        .expect("create_investigation");
    pin_member(
        &db,
        &investigation.id,
        &entity("GB-COH:1"),
        PinOrigin::Profile,
        None,
    )
    .await
    .expect("pin_member");

    for (table, fields) in [
        ("saved_investigation", vec!["created_at", "updated_at"]),
        ("saved_investigation_pin", vec!["pinned_at", "updated_at"]),
    ] {
        // Non-vacuity: `SELECT` over an empty table reports nothing, and nothing is not `false`.
        assert_eq!(
            count(&db, table).await,
            1,
            "{table} must hold exactly one row before its column types mean anything"
        );
        for field in fields {
            let rows: Vec<Value> = db
                .query(format!(
                    "SELECT type::is_datetime({field}) AS is_dt, type::is_string({field}) AS is_str FROM {table}"
                ))
                .await
                .expect("type query")
                .check()
                .expect("type statement")
                .take(0)
                .expect("type decode");
            let row = rows.first().expect("one row");
            assert_eq!(
                row.get("is_dt").and_then(Value::as_bool),
                Some(true),
                "{table}.{field} must be a real datetime in the store, not text"
            );
            assert_eq!(
                row.get("is_str").and_then(Value::as_bool),
                Some(false),
                "{table}.{field} must not be a string"
            );
        }
    }
}

// ── Idempotence, and not clobbering the analyst ──────────────────────────────

/// PIV-05's own wording: "pinning the same entity twice does not create two entries". And the part
/// the wording leaves implicit — the second pin must not overwrite the first one's note or restate
/// when the member was found.
///
/// PROVE THIS CAN FAIL: in `pin_member`, replace
/// `pinned_at = IF type::is_datetime(pinned_at) THEN pinned_at ELSE $now END` with
/// `pinned_at = $now`; the `pinned_at` assertion fails. Replace the `note = IF …` clause with
/// `note = $note` and the write fails outright (NULL into a `string` column) — which is the good
/// kind of failure, and the reason the guard is written that way.
#[tokio::test]
async fn db_pinning_the_same_member_twice_keeps_one_entry_its_note_and_its_first_pinned_at() {
    if !db_tests_enabled() {
        return;
    }
    let db = TestNs::connect("an_test_idempotent").await;
    let investigation = create_investigation(&db, "double pin", "")
        .await
        .expect("create_investigation");
    let id = &investigation.id;
    let target = entity("GB-COH:87654321");

    let first = pin_member(
        &db,
        id,
        &target,
        PinOrigin::Profile,
        Some("found via a planning app"),
    )
    .await
    .expect("first pin");
    assert_eq!(
        count(&db, PIN_TABLE).await,
        1,
        "one pin after the first pin"
    );

    // Re-pinned from a different surface, with no note supplied.
    let second = pin_member(&db, id, &target, PinOrigin::Canvas, None)
        .await
        .expect("second pin");

    assert_eq!(
        count(&db, PIN_TABLE).await,
        1,
        "pinning the same entity twice must not create two entries (PIV-05)"
    );
    assert_eq!(
        second.id, first.id,
        "the same member must resolve to the same record"
    );
    assert_eq!(
        second.note, "found via a planning app",
        "a re-pin with no note must not erase the note the analyst typed"
    );
    assert_eq!(
        second.pinned_at, first.pinned_at,
        "pinned_at records when the member was FIRST found; a re-pin must not restate it"
    );
    assert!(
        second.updated_at >= first.updated_at,
        "updated_at is the one that moves"
    );

    let pins = list_pins(&db, id).await.expect("list_pins");
    assert_eq!(pins.len(), 1, "and the list agrees there is one");
    assert_eq!(pins[0].origin, PinOrigin::Canvas, "the newer origin wins");
}

/// A pin against an investigation that does not exist must fail, and must leave nothing behind.
///
/// The guard is inside the statement: the `record<saved_investigation>` column refuses the `NONE`
/// that the id lookup yields. A dangling member would be invisible in every list and would
/// resurface only in an export.
///
/// PROVE THIS CAN FAIL: change the `investigation` column to `TYPE string` in `SCHEMA_DEF`; the
/// write then succeeds against nothing and both assertions below fail.
#[tokio::test]
async fn db_a_pin_against_a_missing_investigation_fails_and_writes_nothing() {
    if !db_tests_enabled() {
        return;
    }
    let db = TestNs::connect("an_test_dangling").await;

    let outcome = pin_member(
        &db,
        "an-investigation-that-was-never-created",
        &entity("GB-COH:1"),
        PinOrigin::Profile,
        None,
    )
    .await;

    assert!(
        outcome.is_err(),
        "pinning into a missing investigation must be an error, not a quiet success"
    );
    assert_eq!(
        count(&db, PIN_TABLE).await,
        0,
        "and it must not leave a dangling member behind"
    );
}

// ── Notes are work product ───────────────────────────────────────────────────

/// A note saved against something that is not there must not return `Ok`. "Saved" on screen over a
/// write that hit no row is how an analyst loses their own words.
///
/// PROVE THIS CAN FAIL: in `set_pin_note` / `set_investigation_notes`, drop the
/// `.with_context(...)` on the empty-rows case and return `Ok` regardless — both assertions fail.
#[tokio::test]
async fn db_a_note_that_matched_no_row_is_an_error_not_a_silent_ok() {
    if !db_tests_enabled() {
        return;
    }
    let db = TestNs::connect("an_test_notes").await;
    let investigation = create_investigation(&db, "notes", "")
        .await
        .expect("create_investigation");

    assert!(
        set_pin_note(
            &db,
            &investigation.id,
            &entity("GB-COH:never-pinned"),
            "words"
        )
        .await
        .is_err(),
        "a note on an unpinned member must fail loudly"
    );
    assert!(
        set_investigation_notes(&db, "no-such-investigation", "words")
            .await
            .is_err(),
        "notes on a missing investigation must fail loudly"
    );

    // The happy path still works, and the words come back verbatim.
    pin_member(
        &db,
        &investigation.id,
        &entity("GB-COH:1"),
        PinOrigin::Profile,
        None,
    )
    .await
    .expect("pin_member");
    let pin = set_pin_note(
        &db,
        &investigation.id,
        &entity("GB-COH:1"),
        "two directors, one address",
    )
    .await
    .expect("set_pin_note");
    assert_eq!(pin.note, "two directors, one address");

    let reread = list_pins(&db, &investigation.id).await.expect("list_pins");
    assert_eq!(reread.len(), 1);
    assert_eq!(
        reread[0].note, "two directors, one address",
        "the note must be on disk, not only in the returned struct"
    );

    let renamed = rename_investigation(&db, &investigation.id, "  Anglesey, second pass  ")
        .await
        .expect("rename_investigation");
    assert_eq!(renamed.name, "Anglesey, second pass");
    assert!(rename_investigation(&db, &investigation.id, "   ")
        .await
        .is_err());
}

// ── The list, and the counts it shows ────────────────────────────────────────

/// The investigation list shows a name, an entity count and a last-modified time. Edges must not
/// inflate the entity count — a subgraph save can bring hundreds of them — and the most recently
/// touched investigation comes first.
///
/// PROVE THIS CAN FAIL: in `list_investigations`, map both `"entity"` and `"edge"` onto
/// `entity_count`; the count assertion fails.
#[tokio::test]
async fn db_the_list_counts_entities_apart_from_edges_and_orders_by_last_modified() {
    if !db_tests_enabled() {
        return;
    }
    let db = TestNs::connect("an_test_list").await;

    let older = create_investigation(&db, "older", "")
        .await
        .expect("create older");
    let newer = create_investigation(&db, "newer", "")
        .await
        .expect("create newer");

    for n in 1..=3 {
        pin_member(
            &db,
            &older.id,
            &entity(&format!("GB-COH:{n}")),
            PinOrigin::Profile,
            None,
        )
        .await
        .expect("pin entity");
    }
    for n in 1..=2 {
        pin_member(
            &db,
            &older.id,
            &edge(&format!("GB-COH:{n}"), "director:jane-doe", "directs"),
            PinOrigin::Subgraph,
            None,
        )
        .await
        .expect("pin edge");
    }
    assert_eq!(
        count(&db, PIN_TABLE).await,
        5,
        "five members, three of them entities"
    );

    // `newer` was created second but `older` was pinned into last, so `older` is the most
    // recently modified — the column is last-modified, not created.
    let list = list_investigations(&db).await.expect("list_investigations");
    assert_eq!(list.len(), 2, "both investigations must be listed");
    assert_eq!(
        list[0].investigation.id, older.id,
        "the most recently MODIFIED investigation comes first, not the most recently created"
    );
    assert_eq!(
        list[0].entity_count, 3,
        "an entity count must count entities — two edges must not inflate it"
    );
    assert_eq!(list[0].edge_count, 2);
    assert_eq!(list[1].investigation.id, newer.id);
    assert_eq!(list[1].entity_count, 0);
    assert_eq!(list[1].edge_count, 0);
}

// ── Removal ──────────────────────────────────────────────────────────────────

/// Unpinning reports whether it removed anything, and deleting an investigation takes its members
/// with it — an orphaned member is invisible in every list and resurfaces only in an export.
///
/// PROVE THIS CAN FAIL: in `delete_investigation`, drop the statement that deletes the members;
/// the `PIN_TABLE` count assertion fails with the members still there.
#[tokio::test]
async fn db_deleting_an_investigation_takes_its_members_with_it() {
    if !db_tests_enabled() {
        return;
    }
    let db = TestNs::connect("an_test_delete").await;
    let investigation = create_investigation(&db, "to be deleted", "")
        .await
        .expect("create_investigation");
    let id = &investigation.id;

    pin_member(&db, id, &entity("GB-COH:1"), PinOrigin::Profile, None)
        .await
        .expect("pin one");
    pin_member(&db, id, &entity("GB-COH:2"), PinOrigin::Profile, None)
        .await
        .expect("pin two");
    assert_eq!(
        count(&db, PIN_TABLE).await,
        2,
        "two members before anything is removed"
    );

    assert!(
        unpin_member(&db, id, &entity("GB-COH:2"))
            .await
            .expect("unpin"),
        "unpinning a pinned member reports that it removed something"
    );
    assert!(
        !unpin_member(&db, id, &entity("GB-COH:2"))
            .await
            .expect("unpin again"),
        "unpinning it a second time reports that it removed nothing"
    );
    assert_eq!(count(&db, PIN_TABLE).await, 1);

    let removed = delete_investigation(&db, id)
        .await
        .expect("delete_investigation")
        .expect("the investigation existed");
    assert_eq!(removed, 1, "the remaining member is reported as removed");
    assert_eq!(
        count(&db, PIN_TABLE).await,
        0,
        "no member may outlive the investigation that owned it"
    );
    assert!(
        get_investigation(&db, id).await.expect("get").is_none(),
        "and the investigation is gone"
    );
    assert!(
        delete_investigation(&db, id)
            .await
            .expect("delete again")
            .is_none(),
        "deleting it twice reports that there was nothing to delete"
    );
}

// ── An analysis is a database ────────────────────────────────────────────────

/// An analysis id is the **database**, not the namespace. Two analyses in one namespace must not
/// see each other's investigations, and each row must state which analysis it is in — a row that
/// travels into a dossier carries no connection with it.
///
/// PROVE THIS CAN FAIL: in `create_investigation`, replace `analysis_db = session::db()` with a
/// bound caller-supplied string; the second assertion then reflects whatever the caller claimed
/// rather than where the row landed.
#[tokio::test]
async fn db_an_investigation_names_the_analysis_database_it_landed_in() {
    if !db_tests_enabled() {
        return;
    }
    let alpha = TestNs::connect("an_test_alpha").await;
    let ns = alpha.ns().to_string();

    let in_alpha = create_investigation(&alpha, "alpha work", "")
        .await
        .expect("create in alpha");
    assert_eq!(
        in_alpha.analysis_db, "an_test_alpha",
        "the row must name the database it landed in"
    );

    // A second analysis, same namespace, different database.
    let beta = fresh_connection(&ns, "an_test_beta").await;
    let in_beta = create_investigation(&beta, "beta work", "")
        .await
        .expect("create in beta");
    assert_eq!(in_beta.analysis_db, "an_test_beta");

    let alpha_list = list_investigations(&alpha).await.expect("list alpha");
    let beta_list = list_investigations(&beta).await.expect("list beta");
    assert_eq!(alpha_list.len(), 1, "alpha sees exactly its own");
    assert_eq!(beta_list.len(), 1, "beta sees exactly its own");
    assert_eq!(alpha_list[0].investigation.id, in_alpha.id);
    assert_eq!(beta_list[0].investigation.id, in_beta.id);
    assert!(
        get_investigation(&beta, &in_alpha.id)
            .await
            .expect("cross-database read")
            .is_none(),
        "one analysis must not be able to read another's investigation by id"
    );
}
