//! PIV-03 / D-06: a contract, a planning application and an address each carry the graph id of
//! the node they are about, so the profile has something to pivot *to*.
//!
//! # What plan 45-11 Task 1 actually changed
//!
//! Nothing was minted. All three ids were already in scope at the point the row was built — the
//! `AwardedTo` destination for a contract or an application, the `LocatedAt` destination for an
//! address — and were already being used to attribute the row
//! (`graph_event_provenance("Location", lid, …)`). They were simply not carried onto the row, so
//! 45-08 measured a pivot surface two rows wide and pinned it. This file is the evidence that the
//! carrying is verbatim.
//!
//! # The failure this file exists to catch
//!
//! **An address has no natural key.** Postcode, line 1 and locality are each `Option`, and the
//! same three values recur across unrelated entities, so any id derived from them is a link that
//! lands on the wrong node or on nothing. The tempting shortcut — synthesise
//! `format!("location:{postcode}")`, or normalise the two live namespaces into one — produces a
//! fully-populated, entirely clickable profile in which some proportion of the links are wrong,
//! and no count-based test notices.
//!
//! So the load-bearing assertion here is not "an address row has an id" but "the id is
//! **byte-identical** to the `LocatedAt` destination, in whichever namespace that destination was
//! written in". Both namespaces are live: of 173,203 `LocatedAt` destinations on
//! `an_anglesey_aebaa785`, 132,511 are `location:` and 40,692 are `LOC:` (measured 2026-09-15).
//! They are different nodes, not two spellings of one, and [`db_an_address_row_carries_the_located_at_destination_id_verbatim`]
//! seeds one of each so a normalising producer goes red rather than quietly repointing 23% of the
//! addresses on the corpus.
//!
//! # The gap this file pins rather than closes
//!
//! [`db_a_pivot_id_names_a_node_the_profile_route_cannot_open_yet`] asserts that
//! `query_entity_by_id` returns `None` for every one of these ids. That is not a bug being
//! tolerated — it is the honest state after Task 1, and 45-11's Tasks 2–4 need it stated out loud:
//! the ids are real and correct, and the *destination page* does not exist, because
//! `query_entity_by_id` resolves `Entity` and `OffshoreEntity` node weights only. On the live
//! corpus zero of 61,756 `location`-table ids, and zero `CF:`/`planning:` ids, has an `Entity`
//! node (measured 2026-09-15). Rendering these pivots as links before a subject lookup can open a
//! `Location`/`Contract` node ships a profile of 404s. The test fails the day the gap closes,
//! which is when it should be deleted.
//!
//! # Non-vacuity
//!
//! Every section is counted before any id is compared, and the expected id sets are built by the
//! seeder rather than restated, so a run against an empty profile reports a length mismatch
//! instead of "no violations found".
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test profile_pivot_ids
//! ```
//!
//! Gated on `URBAN_DB_TESTS=1`, skipping by returning, so a gated run reports the same passing
//! count as an ungated one and duration is the only witness that a database was reached.
//! Namespaces come from `urban-integrity-test-support`, which drops them.

use std::collections::BTreeSet;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::query_entity_by_id;
use urban_integrity_test_support::{db_tests_enabled, TestNs};

const CH: &str = "X9995001";
const SUBJECT: &str = "GB-COH:X9995001";

const CF_CONTRACT: &str = "CF:x9995-cf-00001";
const PLANNING_APP: &str = "planning:x9995-app-00001";

/// The `location:`-namespaced form, keyed on an address slug. 132,511 of 173,203 `LocatedAt`
/// destinations on the live corpus, minted by the Companies House, Contracts Finder, planning and
/// FCA Mutuals mappers alike.
const LOCATION_SLUG_NS: &str = "location:1-x9995-street,-llangefni";

/// The `LOC:`-namespaced form, keyed on postcode then the raw address line — the other 40,692.
/// Shaped like the real thing, embedded second colon and spaces included
/// (`LOC:AB101JB:KPMG LLP,37 Albyn Place, AB10 1JB` is a live id), because the round trip through
/// SurrealDB's `table:key` RecordId coercion is part of what "verbatim" has to survive.
const LOCATION_POSTCODE_NS: &str = "LOC:LL777AA:2 X9995 Street, Llangefni LL77 7AA";

fn ts() -> u64 {
    1_700_000_000_000
}

fn node_row(event_id: &str, entity_id: &str, weight: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": ts(),
        "kind": "NodeAdded",
        "payload": { "entity_id": entity_id, "weight_json": weight.to_string() },
    })
}

fn edge_row(
    event_id: &str,
    src_id: &str,
    dst_id: &str,
    weight: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": ts(),
        "kind": "EdgeAdded",
        "payload": {
            "src_id": src_id,
            "dst_id": dst_id,
            "weight_json": weight.to_string(),
        },
    })
}

fn edge_weight(kind: &str) -> serde_json::Value {
    serde_json::json!({
        "type": kind,
        "meta": { "created_at": "2024-01-01T00:00:00Z", "effective_from": null, "effective_to": null },
        "data": { "details": "seeded" },
    })
}

fn location_weight(name: &str, postcode: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Location",
        "name": name,
        "postcode": postcode,
        "address_line_1": name,
        "locality": "Llangefni",
    })
}

/// Fail-loud batched write — `INSERT` with a bound array, `.check()`ed. Never a bare `.await?`,
/// which is how the PPD backfill reported "2795151 written" against zero staged rows.
async fn insert_rows(db: &Surreal<Any>, rows: Vec<serde_json::Value>) {
    for chunk in rows.chunks(500) {
        db.query("INSERT INTO graph_events $rows")
            .bind(("rows", serde_json::Value::Array(chunk.to_vec())))
            .await
            .expect("seed batch: query")
            .check()
            .expect("seed batch: statement");
    }
}

async fn seed(db: &Surreal<Any>) {
    let rows = vec![
        node_row(
            "x9995-subject",
            SUBJECT,
            serde_json::json!({ "type": "Entity", "name": "X9995 Subject Ltd",
                                "company_number": CH, "status": { "type": "Active" } }),
        ),
        node_row(
            "x9995-cf-node",
            CF_CONTRACT,
            serde_json::json!({ "type": "Contract", "name": "X9995 CF Award",
                                "status": "awarded", "ward_code": "X05999001",
                                "tender_ref": "X9995-TENDER-1", "value_gbp": 125000.0 }),
        ),
        edge_row(
            "x9995-cf-edge",
            SUBJECT,
            CF_CONTRACT,
            edge_weight("AwardedTo"),
        ),
        node_row(
            "x9995-app-node",
            PLANNING_APP,
            serde_json::json!({ "type": "Contract", "name": "X9995 Planning Application",
                                "status": "pending", "ward_code": "X05999001",
                                "application_ref": "X9995C001" }),
        ),
        edge_row(
            "x9995-app-edge",
            SUBJECT,
            PLANNING_APP,
            edge_weight("AwardedTo"),
        ),
        node_row(
            "x9995-loc-slug-node",
            LOCATION_SLUG_NS,
            location_weight("1 X9995 Street", "LL77 7AA"),
        ),
        edge_row(
            "x9995-loc-slug-edge",
            SUBJECT,
            LOCATION_SLUG_NS,
            edge_weight("LocatedAt"),
        ),
        node_row(
            "x9995-loc-pc-node",
            LOCATION_POSTCODE_NS,
            location_weight("2 X9995 Street", "LL77 7AA"),
        ),
        edge_row(
            "x9995-loc-pc-edge",
            SUBJECT,
            LOCATION_POSTCODE_NS,
            edge_weight("LocatedAt"),
        ),
    ];
    insert_rows(db, rows).await;
}

/// A contract row and a planning-application row each carry the `AwardedTo` destination id they
/// were assembled from.
///
/// PROVE THIS CAN FAIL: in `assemble_entity_profile`, set `id: String::new()` on the `ContractRow`
/// literal. The contract assertion fails with `"" != "CF:x9995-cf-00001"`. That mutation was run.
#[tokio::test]
async fn db_contract_and_planning_rows_carry_the_awarded_to_destination_id() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_contract_and_planning_rows_carry_the_awarded_to_destination_id \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_pivot_ids_awarded").await;
    seed(&db).await;

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("query_entity_by_id")
        .expect("the seeded subject must resolve");

    // Non-vacuity: both sections were populated, so the comparisons below compare something.
    assert_eq!(
        profile.contracts.len(),
        1,
        "the fixture seeds exactly one CF award; {} came back",
        profile.contracts.len()
    );
    assert_eq!(
        profile.planning_applications.len(),
        1,
        "the fixture seeds exactly one planning application; {} came back",
        profile.planning_applications.len()
    );

    assert_eq!(
        profile.contracts[0].id, CF_CONTRACT,
        "a contract row's id is the AwardedTo destination, carried not derived — note it is NOT \
         the tender_ref, which is a Contracts Finder reference and not a graph node id"
    );
    assert_eq!(
        profile.planning_applications[0].id, PLANNING_APP,
        "a planning application's id is its own `planning:` destination, which is also the \
         predicate that put it in this section rather than in contracts"
    );

    // The id is the node key, not the payload reference. A producer that reached for the
    // human-facing reference instead would pass every length check above.
    assert_ne!(
        profile.contracts[0].id,
        profile.contracts[0].tender_ref.clone().unwrap_or_default(),
        "tender_ref is not a node id"
    );
    assert_ne!(
        profile.planning_applications[0].id,
        profile.planning_applications[0]
            .application_ref
            .clone()
            .unwrap_or_default(),
        "application_ref is not a node id"
    );
}

/// An address row carries the `LocatedAt` destination id **verbatim**, in whichever of the two
/// live namespaces the edge was written in.
///
/// PROVE THIS CAN FAIL: in `assemble_entity_profile`, replace the `AddressRow` literal's
/// `id: lid.clone()` with a synthesised one —
/// `id: format!("location:{}", ld.postcode.clone().unwrap_or_default())` — which is the exact
/// shortcut this test exists to forbid. Both seeded addresses then come back as
/// `location:LL77 7AA` and the assertion fails on the expected-set comparison. That mutation was
/// run.
#[tokio::test]
async fn db_an_address_row_carries_the_located_at_destination_id_verbatim() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_an_address_row_carries_the_located_at_destination_id_verbatim \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_pivot_ids_address").await;
    seed(&db).await;

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("query_entity_by_id")
        .expect("the seeded subject must resolve");

    // Non-vacuity, counted before anything is compared.
    assert_eq!(
        profile.addresses.len(),
        2,
        "the fixture seeds one `location:` and one `LOC:` destination; {} came back",
        profile.addresses.len()
    );

    let got: BTreeSet<String> = profile.addresses.iter().map(|a| a.id.clone()).collect();
    let expected: BTreeSet<String> = [LOCATION_SLUG_NS.to_owned(), LOCATION_POSTCODE_NS.to_owned()]
        .into_iter()
        .collect();
    assert_eq!(
        got, expected,
        "both address ids must be the LocatedAt destinations byte-for-byte. A normalised or \
         synthesised id points at a node that does not exist: `location:` and `LOC:` are 132,511 \
         and 40,692 distinct destinations on the live corpus, not two spellings of one"
    );

    // Every row has one: the read path cannot build an address row without reading the Location
    // node the id names, so `id` is not an `Option` and must never arrive empty.
    assert!(
        profile.addresses.iter().all(|a| !a.id.is_empty()),
        "an address row with an empty id is unconstructible from the read path"
    );

    // The postcode is identical on both seeded rows, so an id derived from it could not tell them
    // apart. Stated as an assertion rather than left to the fixture's good intentions.
    assert_eq!(
        profile
            .addresses
            .iter()
            .filter_map(|a| a.postcode.clone())
            .collect::<BTreeSet<_>>()
            .len(),
        1,
        "both fixture addresses share one postcode — that is the point: a postcode-derived id \
         would collapse two distinct nodes into one link"
    );
}

/// **A pin, not a guard.** Every id the profile now offers as a pivot target names a node
/// `query_entity_by_id` cannot open, because it resolves `Entity` and `OffshoreEntity` node
/// weights only.
///
/// This is the state 45-11's Tasks 2–4 and plan 45-10 inherit: the ids are correct and the
/// destination page does not exist. A renderer that turns these into links today produces a
/// profile of 404s, which is the "dead link" failure 45-08 declined to create by inventing ids and
/// which must not be reintroduced by rendering real ones too early.
///
/// **It fails the day the gap closes** — when a subject lookup learns to assemble a
/// `Location`/`Contract` profile, delete it. That is the same signal 45-08's
/// `pivot_coverage_is_what_the_read_path_can_supply` carried, one layer down.
///
/// PROVE THIS CAN FAIL: seed an `Entity`-weighted `NodeAdded` under `CF:x9995-cf-00001` and the
/// contract assertion fails, because the id then does resolve. That mutation was run.
#[tokio::test]
async fn db_a_pivot_id_names_a_node_the_profile_route_cannot_open_yet() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_a_pivot_id_names_a_node_the_profile_route_cannot_open_yet \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_pivot_ids_unopenable").await;
    seed(&db).await;

    // Non-vacuity: the subject itself resolves, so a `None` below is about the pivot target's node
    // class and not about the fixture failing to seed or the namespace being empty.
    assert!(
        query_entity_by_id(&db, SUBJECT)
            .await
            .expect("query_entity_by_id")
            .is_some(),
        "the subject must resolve, or the Nones below prove nothing"
    );

    for id in [
        CF_CONTRACT,
        PLANNING_APP,
        LOCATION_SLUG_NS,
        LOCATION_POSTCODE_NS,
    ] {
        let opened = query_entity_by_id(&db, id)
            .await
            .expect("query_entity_by_id");
        assert!(
            opened.is_none(),
            "{id} resolved to a profile. If a subject lookup for Location/Contract nodes has \
             landed, this pin has done its job — delete it, and tell 45-11's Task 2 that the \
             contract and address pivots are now live links rather than ids waiting for a page"
        );
    }
}
