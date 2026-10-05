//! PIV-06: the entity profile and the neighbour read path are **complete**, and stay complete.
//!
//! A capped query looks exactly like a complete one. It returns `Ok`, it returns rows, the rows
//! are real, and nothing anywhere says there were more. That is why PIV-06 asks for a test rather
//! than a code review, and why this file's job is not "does the profile work" but "does the
//! profile return *everything*, over an entity big enough that a cap would bite".
//!
//! # What this file would catch
//!
//! Every literal plan 45-02 removed is asserted against here, by seeding a subject whose
//! population is larger than all of them and comparing sets rather than counts alone:
//!
//! | Removed literal | Where it was | Population seeded here |
//! |---|---|---|
//! | `LIMIT 5` per contract | `assemble_entity_profile`, `query_entity_lpa_presence` | a contract with **6** node-weight events |
//! | `.take(50)` | the two alert sections | — (asserted structurally, see below) |
//! | related-entities `limit = 10` | `assemble_entity_profile` | 2,100 shared-officer relatives |
//! | `LIMIT 20000` / `LIMIT 100000` | AwardedTo edges, five relation tables | — (GRD-09 paging, exercised by the scans below) |
//!
//! # Non-vacuity
//!
//! `assert_eq!(returned, expected)` is a passing test when both are empty, and this project has
//! been bitten by exactly that (the phase 44 verification report's vacuous diffs). So **every comparison in
//! this file is preceded by an assertion that the population is non-zero and above the threshold
//! the requirement names.** The seeded set is also cross-checked against a `count()` issued
//! independently against the database, so neither side of the comparison is merely the seeder's
//! own bookkeeping being read back.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test profile_paging
//! ```
//!
//! Gated on `URBAN_DB_TESTS=1` **and** a reachable SurrealDB, and skips by returning — so an
//! ungated run reports the same passing count as a gated one. Duration is the only observable
//! difference; a gated run of this file that finishes instantly did not reach a database.
//! Namespaces come from `urban-integrity-test-support`, which removes them on drop.

use std::collections::{BTreeMap, BTreeSet};

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    query_entity_by_id, query_entity_neighbours, query_related_entities_complete, Neighbour,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};

/// The subject. Synthetic `X9999…` numbering, never a live analysis id.
const SUBJECT: &str = "GB-COH:X9999001";

/// Officers directing the subject. Above 2,000 on its own, so the neighbour population clears
/// PIV-06's threshold even if the contract block were removed.
const N_OFFICERS: usize = 2_100;

/// Contracts awarded to the subject.
const N_CONTRACTS: usize = 400;

/// Companies sharing a director with the subject — the `related_entities` section, whose D-10
/// ceiling of 10 this plan retired. Chosen above every ceiling in [`REMOVED_CEILINGS`] so
/// reintroducing any of them is visible here, not just arithmetically implied.
const N_RELATIVES: usize = 120;

/// PIV-06's threshold: "a profile over a 2,000+-neighbour entity is complete".
const NEIGHBOUR_THRESHOLD: usize = 2_000;

/// Every ceiling plan 45-02 removed from the profile read path. The test asserts the seeded
/// population is strictly greater than each of them, so "a cap was reintroduced" and "this test
/// fails" are the same event rather than two things that happen to coincide today.
const REMOVED_CEILINGS: &[(&str, usize)] = &[
    ("related-entities limit (D-10)", 10),
    ("alerts .take(50)", 50),
    ("osint_links LIMIT 100", 100),
    ("per-contract LIMIT 5", 5),
];

fn officer_id(i: usize) -> String {
    format!("director:x9999-off-{i:05}")
}

fn officer_name(i: usize) -> String {
    format!("X9999 Officer {i:05}")
}

fn contract_id(i: usize) -> String {
    format!("contract:x9999-con-{i:05}")
}

fn contract_name(i: usize) -> String {
    format!("X9999 Contract {i:05}")
}

fn relative_id(i: usize) -> String {
    format!("GB-COH:X9999R{i:04}")
}

fn relative_name(i: usize) -> String {
    format!("X9999 Shared-Director Co {i:04}")
}

/// The contract deliberately given SIX node-weight events — one `NodeAdded` and five later
/// `NodeUpdated`s. Under the `… payload.entity_id = $eid LIMIT 5` point read this replaces, the
/// query had no `ORDER BY`, so *which* five rows came back was arbitrary and the single
/// `NodeAdded` could fall outside them — at which point the contract vanished from the profile
/// with no error. The re-ingest also used to render the contract once per returned row.
const REINGESTED_INDEX: usize = 7;

/// The name carried by the newest of the re-ingested contract's six events.
const REINGESTED_LATEST_NAME: &str = "X9999 Contract 00007 (sixth revision)";

fn base_timestamp() -> u64 {
    1_700_000_000_000
}

/// One `graph_events` row, ready for a batched `INSERT`.
fn node_row(
    event_id: &str,
    entity_id: &str,
    weight: serde_json::Value,
    ts: u64,
) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": ts,
        "kind": "NodeAdded",
        "payload": { "entity_id": entity_id, "weight_json": weight.to_string() },
    })
}

fn node_update_row(
    event_id: &str,
    entity_id: &str,
    weight: serde_json::Value,
    ts: u64,
) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": ts,
        "kind": "NodeUpdated",
        "payload": { "entity_id": entity_id, "weight_json": weight.to_string() },
    })
}

fn edge_row(
    event_id: &str,
    src_id: &str,
    dst_id: &str,
    weight: serde_json::Value,
    ts: u64,
) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": ts,
        "kind": "EdgeAdded",
        "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight.to_string() },
    })
}

fn entity_weight(name: &str, company_number: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Entity",
        "name": name,
        "company_number": company_number,
        // EntityStatus is `#[serde(tag = "type", content = "content")]`, so the wire form of
        // Active is an object, not the bare string a fixture is tempted to write.
        "status": { "type": "Active" },
    })
}

fn officer_weight(name: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Officer",
        "name": name,
        "officer_role": "director",
        "appointed_on": "2020-01-01",
    })
}

fn contract_weight(name: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Contract",
        "name": name,
        "value_gbp": 1000.0,
        "status": "active",
        "date_published": "2024-01-01",
        "tender_ref": "X9999-TENDER",
    })
}

fn directs_weight() -> serde_json::Value {
    serde_json::json!({
        "type": "Directs",
        "meta": { "created_at": "2020-01-01T00:00:00Z", "effective_from": null, "effective_to": null },
        "data": { "details": "seeded", "officer_role": "director" },
    })
}

fn awarded_weight() -> serde_json::Value {
    serde_json::json!({
        "type": "AwardedTo",
        "meta": { "created_at": "2024-01-01T00:00:00Z", "effective_from": null, "effective_to": null },
        "data": { "value_gbp": 1000.0, "award_date": "2024-01-01" },
    })
}

/// Fail-loud batched write. `INSERT` with a bound array, `.check()`ed — never a bare `.await?`,
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

/// Seed the subject, its officers and its contracts. Returns the two id sets the assertions
/// compare against.
async fn seed(db: &Surreal<Any>) -> (BTreeSet<String>, BTreeSet<String>) {
    let ts = base_timestamp();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut officer_ids: BTreeSet<String> = BTreeSet::new();
    let mut contract_ids: BTreeSet<String> = BTreeSet::new();

    rows.push(node_row(
        "x9999-subject",
        SUBJECT,
        entity_weight("X9999 Subject Ltd", "X9999001"),
        ts,
    ));

    for i in 0..N_OFFICERS {
        let oid = officer_id(i);
        rows.push(node_row(
            &format!("x9999-off-node-{i}"),
            &oid,
            officer_weight(&officer_name(i)),
            ts,
        ));
        // Officer -> subject: an INBOUND edge. Under the settled neighbour definition
        // (the phase 45 design notes, open question 3) this makes the officer a neighbour; a direction-only
        // reading of "neighbour" would return zero here, which is why the direction is seeded
        // this way rather than outward.
        rows.push(edge_row(
            &format!("x9999-off-edge-{i}"),
            &oid,
            SUBJECT,
            directs_weight(),
            ts,
        ));
        officer_ids.insert(oid.clone());

        // The first N_RELATIVES officers also direct a company of their own, which makes that
        // company a *related entity* of the subject (shared director) without making it a
        // neighbour — it is two hops away. This is the population the retired D-10 ceiling of 10
        // used to truncate.
        if i < N_RELATIVES {
            let rid = relative_id(i);
            rows.push(node_row(
                &format!("x9999-rel-node-{i}"),
                &rid,
                entity_weight(&relative_name(i), &format!("X9999R{i:04}")),
                ts,
            ));
            rows.push(edge_row(
                &format!("x9999-rel-edge-{i}"),
                &oid,
                &rid,
                directs_weight(),
                ts,
            ));
        }
    }

    for i in 0..N_CONTRACTS {
        let cid = contract_id(i);
        rows.push(node_row(
            &format!("x9999-con-node-{i}"),
            &cid,
            contract_weight(&contract_name(i)),
            ts,
        ));
        rows.push(edge_row(
            &format!("x9999-con-edge-{i}"),
            SUBJECT,
            &cid,
            awarded_weight(),
            ts,
        ));
        contract_ids.insert(cid);
    }

    // The re-ingested contract: five later NodeUpdated events on top of its NodeAdded, so it
    // carries six node-weight rows in total.
    let reingested = contract_id(REINGESTED_INDEX);
    for rev in 1..=5 {
        let name = if rev == 5 {
            REINGESTED_LATEST_NAME.to_string()
        } else {
            format!("X9999 Contract {REINGESTED_INDEX:05} (revision {rev})")
        };
        rows.push(node_update_row(
            &format!("x9999-con-node-{REINGESTED_INDEX}-rev-{rev}"),
            &reingested,
            contract_weight(&name),
            ts + rev as u64,
        ));
    }

    insert_rows(db, rows).await;
    (officer_ids, contract_ids)
}

/// A `count()` issued against the database, independent of the seeder's own bookkeeping.
///
/// `count()` is the one aggregate the SurrealDB 3.x planner bug leaves correct — it is what stays
/// right while the row-returning form truncates — so it is the right second opinion here.
async fn count_events_of_kind(db: &Surreal<Any>, kind: &str) -> usize {
    let n: Option<i64> = db
        .query("SELECT count() FROM graph_events WHERE kind = $kind GROUP ALL")
        .bind(("kind", kind.to_owned()))
        .await
        .expect("count query")
        .take((0, "count"))
        .expect("count take");
    n.unwrap_or(0) as usize
}

/// `count()` of the `EdgeAdded` rows the given node is an endpoint of, in either direction.
///
/// The second opinion on the neighbour population. A `count()` is the one aggregate the
/// SurrealDB 3.x planner bug leaves correct, and equality on `payload.src_id`/`dst_id` with no
/// `CONTAINS` alongside it is not the truncating shape.
async fn count_edges_touching(db: &Surreal<Any>, node_id: &str) -> usize {
    let n: Option<i64> = db
        .query(
            "SELECT count() FROM graph_events \
             WHERE kind = 'EdgeAdded' AND (payload.src_id = $id OR payload.dst_id = $id) \
             GROUP ALL",
        )
        .bind(("id", node_id.to_owned()))
        .await
        .expect("adjacency count query")
        .take((0, "count"))
        .expect("adjacency count take");
    n.unwrap_or(0) as usize
}

/// PIV-06's headline: the neighbour read path over a 2,000+-neighbour entity is complete.
#[tokio::test]
async fn db_neighbours_of_a_2000_plus_neighbour_entity_are_complete() {
    if !db_tests_enabled() {
        eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
        return;
    }
    let db = TestNs::connect("profile_paging_neighbours").await;
    let (officer_ids, contract_ids) = seed(&db).await;

    // ── Population, measured on the seeded side ──────────────────────────────
    let expected: BTreeSet<String> = officer_ids.union(&contract_ids).cloned().collect();
    assert_eq!(
        expected.len(),
        N_OFFICERS + N_CONTRACTS,
        "the seed must produce distinct ids on both sides"
    );
    assert!(
        expected.len() > NEIGHBOUR_THRESHOLD,
        "NON-VACUITY: PIV-06 is about a {NEIGHBOUR_THRESHOLD}+-neighbour entity, and this test \
         seeded only {}. Below the threshold the comparison below proves nothing about the \
         requirement.",
        expected.len()
    );

    // ── Population, measured independently against the database ──────────────
    // If the two disagree, the seed did not land, and every assertion after this point would be
    // comparing the read path against a fiction.
    let edge_events = count_edges_touching(&db, SUBJECT).await;
    assert_eq!(
        edge_events,
        expected.len(),
        "the database's own count() must agree with the seeded set — {} edges expected, count() \
         reports {edge_events}. A disagreement means the batched INSERT partially failed, which \
         is the silent-write class this project has been bitten by twice.",
        expected.len()
    );
    // The fixture also holds edges that do NOT touch the subject (each relative's own
    // directorship), so the adjacency count above is a real filter rather than a row total.
    assert!(
        count_events_of_kind(&db, "EdgeAdded").await > edge_events,
        "the fixture must contain edges the subject is not an endpoint of, or the adjacency \
         count proves nothing about the subject"
    );

    // ── The read ─────────────────────────────────────────────────────────────
    let neighbours = query_entity_neighbours(&db, SUBJECT)
        .await
        .expect("query_entity_neighbours");
    let got: BTreeSet<String> = neighbours.iter().map(|n| n.id.clone()).collect();

    assert!(
        !got.is_empty(),
        "NON-VACUITY: the read returned nothing, so set equality below could only pass by \
         comparing two empty sets"
    );
    assert_eq!(
        got.len(),
        neighbours.len(),
        "a neighbour must appear once, however many edges reach it"
    );

    // ── The comparison ───────────────────────────────────────────────────────
    let missing: Vec<&String> = expected.difference(&got).collect();
    let extra: Vec<&String> = got.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the neighbour read is not complete. {} missing (first few: {:?}), {} unexpected (first \
         few: {:?}). Missing neighbours are the cap-reintroduced signature: a ceiling anywhere on \
         this path returns a prefix and reports success.",
        missing.len(),
        missing.iter().take(5).collect::<Vec<_>>(),
        extra.len(),
        extra.iter().take(5).collect::<Vec<_>>(),
    );

    // ── The falsifier ────────────────────────────────────────────────────────
    // Every ceiling 45-02 removed is strictly below this population, so reintroducing any of them
    // MUST drop neighbours and MUST fail the comparison above. Stated as an assertion rather than
    // a comment so that shrinking the fixture below a ceiling fails here instead of silently
    // making the test unable to detect that ceiling.
    for (name, ceiling) in REMOVED_CEILINGS {
        assert!(
            *ceiling < got.len(),
            "this test can no longer detect the `{name}` ceiling of {ceiling}: the seeded \
             population is only {}. Either raise the fixture or remove the entry.",
            got.len()
        );
    }

    // And demonstrate the drop concretely rather than only by arithmetic: a capped view of the
    // very same data omits neighbours the complete read returned.
    let capped: BTreeSet<String> = got.iter().take(50).cloned().collect();
    assert!(
        capped.len() < got.len(),
        "a 50-row cap must be a strict subset of a {}-neighbour population",
        got.len()
    );
    assert!(
        got.difference(&capped).count() >= NEIGHBOUR_THRESHOLD,
        "a 50-row cap hides at least {NEIGHBOUR_THRESHOLD} of this entity's neighbours — this is \
         what the profile did before PIV-06"
    );

    // ── Determinism ──────────────────────────────────────────────────────────
    // `graph_events` record ids are random, so a read that did not impose an order would come
    // back differently between two calls over identical data.
    let again = query_entity_neighbours(&db, SUBJECT)
        .await
        .expect("query_entity_neighbours (second call)");
    assert_eq!(
        neighbours, again,
        "the neighbour read must be deterministic — same rows, same order"
    );
    let mut sorted = neighbours.clone();
    sorted.sort();
    assert_eq!(
        neighbours, sorted,
        "the returned order must be the documented one (by neighbour id), not scan order"
    );

    // Every neighbour carries the edge type that made it one, so an analyst pivoting can see
    // whether they are crossing a recorded relationship or a resolution.
    let officer_neighbour = neighbours
        .iter()
        .find(|n| n.id == officer_id(0))
        .expect("the first officer is a neighbour");
    assert_eq!(
        officer_neighbour,
        &Neighbour {
            id: officer_id(0),
            edge_types: vec!["Directs".to_string()],
        },
        "an inbound Directs edge makes the officer a neighbour, labelled with its edge type"
    );
}

/// The profile itself — every section over the same 2,000+-neighbour subject.
#[tokio::test]
async fn db_profile_sections_of_a_2000_plus_neighbour_entity_are_complete() {
    if !db_tests_enabled() {
        eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
        return;
    }
    let db = TestNs::connect("profile_paging_sections").await;
    let (officer_ids, contract_ids) = seed(&db).await;

    assert!(
        officer_ids.len() > NEIGHBOUR_THRESHOLD,
        "NON-VACUITY: the directors assertion below is only about PIV-06 if the subject has more \
         than {NEIGHBOUR_THRESHOLD} of them; it has {}",
        officer_ids.len()
    );

    // Second opinion on the population, same reasoning as the neighbour test.
    let node_events = count_events_of_kind(&db, "NodeAdded").await;
    assert_eq!(
        node_events,
        1 + officer_ids.len() + contract_ids.len() + N_RELATIVES,
        "the database's own count() must agree with the seeded node set"
    );

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("query_entity_by_id")
        .expect("the subject exists");

    // ── Directors ────────────────────────────────────────────────────────────
    assert!(
        !profile.directors.is_empty(),
        "NON-VACUITY: no directors returned, so the comparison below would compare two empty sets"
    );
    let got_directors: BTreeSet<String> =
        profile.directors.iter().map(|d| d.name.clone()).collect();
    let expected_directors: BTreeSet<String> = (0..N_OFFICERS).map(officer_name).collect();
    assert_eq!(
        got_directors.len(),
        expected_directors.len(),
        "every seeded officer must appear exactly once: expected {} distinct, got {} distinct \
         from {} rows",
        expected_directors.len(),
        got_directors.len(),
        profile.directors.len()
    );
    assert_eq!(
        got_directors, expected_directors,
        "the directors section is not complete — a cap on the Directs read returns a prefix"
    );
    assert_eq!(
        profile.directors.len(),
        N_OFFICERS,
        "no duplicates: an append-only log re-emits Directs edges, and the profile used to render \
         the officer once per edge"
    );

    // Deterministic order. Officer names sort in the same order as the officer ids that produced
    // them, so a sorted name list is the observable form of "sorted by officer id".
    let names: Vec<String> = profile.directors.iter().map(|d| d.name.clone()).collect();
    let mut sorted_names = names.clone();
    sorted_names.sort();
    assert_eq!(
        names, sorted_names,
        "directors must come back in a deterministic order, not graph_events scan order"
    );

    // Tenure survived the rewrite (D-02: PIV-02 and PIV-04 both depend on it).
    assert!(
        profile
            .directors
            .iter()
            .all(|d| d.appointed.as_deref() == Some("2020-01-01")),
        "every director carries the tenure date its Officer node was seeded with"
    );

    // ── Contracts ────────────────────────────────────────────────────────────
    assert!(
        !profile.contracts.is_empty(),
        "NON-VACUITY: no contracts returned"
    );
    assert_eq!(
        profile.contracts.len(),
        N_CONTRACTS,
        "every awarded contract appears exactly once — including the one with six node-weight \
         events, which the per-contract `LIMIT 5` could drop entirely and which the missing fold \
         used to render once per returned row"
    );

    // The re-ingested contract is present exactly once, showing its NEWEST weight. Under the
    // shape this replaces it could be absent (its single NodeAdded falling outside an unordered
    // five), duplicated, or stale (its birth weight rendered over five later revisions).
    let reingested: Vec<&str> = profile
        .contracts
        .iter()
        .map(|c| c.name.as_str())
        .filter(|n| n.starts_with(&format!("X9999 Contract {REINGESTED_INDEX:05}")))
        .collect();
    assert_eq!(
        reingested,
        vec![REINGESTED_LATEST_NAME],
        "the six-event contract must appear exactly once, folded to its newest weight"
    );

    // ── Related entities (the retired D-10 cap of 10) ────────────────────────
    assert!(
        !profile.related_entities.is_empty(),
        "NON-VACUITY: no related entities returned, so the comparison below would compare two \
         empty sets"
    );
    for (name, ceiling) in REMOVED_CEILINGS {
        assert!(
            *ceiling < N_RELATIVES,
            "this assertion can no longer detect the `{name}` ceiling of {ceiling}: only \
             {N_RELATIVES} relatives are seeded"
        );
    }
    let got_relatives: BTreeSet<String> = profile
        .related_entities
        .iter()
        .map(|r| r.name.clone())
        .collect();
    let expected_relatives: BTreeSet<String> = (0..N_RELATIVES).map(relative_name).collect();
    assert_eq!(
        got_relatives, expected_relatives,
        "the related-entities section is not complete. It carried a hard ceiling of 10 until \
         PIV-06, and build_verify_verdict counts Dissolved(_) entries in this very vector — so a \
         Phoenix pattern made of dissolved relatives was capped at ten at exactly the point an \
         analyst would look for it."
    );
    assert!(
        profile
            .related_entities
            .iter()
            .all(|r| r.shared_connection.kind == "director"),
        "every relative here is shared-director, and the shared officer must be named"
    );
    assert!(
        profile
            .related_entities
            .iter()
            .all(|r| r.shared_connection.value.starts_with("X9999 Officer ")),
        "the shared officer's NAME must resolve — the hoisted officer map replaced a per-result \
         full table scan, and a map miss would degrade to an empty string rather than error"
    );

    // The profile's section and the standalone complete read are the same read.
    let related = query_related_entities_complete(&db, SUBJECT)
        .await
        .expect("query_related_entities_complete");
    assert_eq!(
        related.len(),
        profile.related_entities.len(),
        "the profile's related-entities section must be the complete read, not a windowed one"
    );

    // ── Alerts (the retired .take(50)) ───────────────────────────────────────
    // None are seeded, so the honest assertion is the structural one: the section is empty
    // because there are no MotifDetected events, not because a filter emptied it.
    assert_eq!(count_events_of_kind(&db, "MotifDetected").await, 0);
    assert!(profile.alerts.is_empty());
    assert!(profile.osint_alerts.is_empty());
}

/// The paged relation-table scan reads past its own page size.
///
/// `owns_property` and its four siblings were read with a bare `LIMIT 100000`. This is the same
/// A/B shape `scan.rs`'s own paging tests use: seed more rows than one page holds and assert the
/// paged read returns all of them.
#[tokio::test]
async fn db_relation_scan_pages_past_its_page_size() {
    if !db_tests_enabled() {
        eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
        return;
    }
    let db = TestNs::connect("profile_paging_relations").await;

    // 150 owns_property edges from one owner, read with a page size of 40 — four pages.
    const N: usize = 150;
    const PAGE: usize = 40;
    for i in 0..N {
        let owner = format!("entity:`{SUBJECT}`");
        let prop = format!("property:`X9999-title-{i:05}`");
        db.query(format!(
            "RELATE {owner}->owns_property->{prop} CONTENT {{ \
                 meta: {{ \
                     created_at: d'2024-04-12T00:00:00Z', \
                     effective_from: d'2024-04-12T00:00:00Z', \
                     effective_to: NONE \
                 }}, \
                 data: {{ price_paid: $price, tenure: 'Freehold' }} \
             }} RETURN NONE"
        ))
        .bind(("price", 100_000.0 + i as f64))
        .await
        .expect("relate")
        .check()
        .expect("relate statement");
    }

    let rows = urban_integrity_core::persistence::scan::scan_relation_rows_paged(
        &db,
        "owns_property",
        &[("data.price_paid", "price_paid")],
        None,
        PAGE,
    )
    .await
    .expect("scan_relation_rows_paged");

    // NON-VACUITY, enforced at BUILD time rather than at run time (the `caps.rs` idiom plan
    // 45-01 established): a fixture that fits in one page proves nothing about paging, and
    // shrinking it should fail the compile rather than quietly weaken the test.
    const { assert!(N > PAGE) };
    assert_eq!(
        rows.len(),
        N,
        "the paged relation scan must read past its page size — got {} of {N}",
        rows.len()
    );

    // The projection carries the extra field, and the ids are distinct: a paging loop that failed
    // to advance `START` would return the first page repeatedly, which a length check alone
    // cannot distinguish from a correct read.
    let distinct: BTreeSet<String> = rows.iter().map(|r| r["out_str"].to_string()).collect();
    assert_eq!(
        distinct.len(),
        N,
        "every row must be a distinct edge — a loop that re-read page one would repeat ids"
    );
    let priced: BTreeMap<String, f64> = rows
        .iter()
        .filter_map(|r| Some((r["out_str"].to_string(), r["price_paid"].as_f64()?)))
        .collect();
    assert_eq!(
        priced.len(),
        N,
        "the (path, alias) projection must reach every row"
    );

    // The in-side filter narrows to one owner rather than to one page.
    let filtered = urban_integrity_core::persistence::scan::scan_relation_rows_paged(
        &db,
        "owns_property",
        &[],
        Some((
            urban_integrity_core::persistence::scan::RelationSide::In,
            &format!("entity:`{SUBJECT}`"),
        )),
        PAGE,
    )
    .await
    .expect("filtered scan");
    assert_eq!(
        filtered.len(),
        N,
        "the SQL-side `in` filter must match the owner every edge was seeded from"
    );
}
