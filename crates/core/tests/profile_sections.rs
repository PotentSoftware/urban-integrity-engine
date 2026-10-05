//! PIV-02 / D-03: contracts and planning applications are two sections, and stay two.
//!
//! # The failure this file exists to catch
//!
//! Both kinds of thing reach an entity over the same `AwardedTo` edge, so the split is one
//! predicate and it is easy to get backwards. The plan that specified this work, and the context
//! document behind it, both originally framed the split as "contracts are the destinations
//! prefixed `contract:`". **That prefix does not occur in this project's store at all.** Measured
//! against `an_anglesey_aebaa785` on 2026-09-15, the 45,876 distinct `Contract` nodes are 39,860
//! `planning:` (86.9%) and 6,016 `CF:` (13.1%), and zero anything else.
//!
//! Written that way the change fails in the worst available shape: the contracts section empties,
//! the planning applications stay exactly where they were, and an assertion that "no planning
//! reference appears under contracts" **still passes**, because it is comparing an empty section
//! against an empty section. So this file:
//!
//! 1. splits on `planning:` as the positive predicate and asserts that a `contract:`-prefixed
//!    destination is classified as a **contract** — the one assertion that goes red on the
//!    backwards predicate rather than silently green;
//! 2. counts both populations before comparing anything, and asserts both are non-empty;
//! 3. asserts the split is a **partition** — every seeded destination appears in exactly one
//!    section, so a row cannot be lost between them or counted in both.
//!
//! # A live fact that shapes the fixture
//!
//! **No entity on the live corpus holds both kinds.** Every `planning:` destination is reached
//! from a `director:` applicant slug and every `CF:` one from a `GB-COH:` or `CF-PARTY:` supplier:
//! of 25,314 `AwardedTo` sources, 20,660 are planning-only, 4,654 are contract-only and **0 hold
//! both** (measured 2026-09-15). The two populations are disjoint by how the ingest mappers key
//! their nodes, not by coincidence, and they stop being disjoint the moment identity resolution
//! joins an applicant slug to a company.
//!
//! That is why the "both sections populated" invariant can only be checked against a seeded
//! fixture: there is no live subject that would exercise it. It is also why the expectation in the
//! plan — "a subject with a four-figure contract count ends with a three-figure contracts section"
//! — does not hold on this corpus. Today every subject moves wholly to one side, so a contracts
//! section going to zero is the **correct** result for a planning-only applicant and is not, on its
//! own, evidence of a mismatched prefix. The `contract:`-prefixed trap destination below is what
//! distinguishes the two cases.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test profile_sections
//! ```
//!
//! Gated on `URBAN_DB_TESTS=1` **and** a reachable SurrealDB, skipping by returning, so a gated
//! run reports the same passing count as an ungated one and duration is the only witness that a
//! database was reached. Namespaces come from `urban-integrity-test-support`, which drops them.

use std::collections::BTreeSet;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    is_planning_application_id, query_entity_by_id, PLANNING_APPLICATION_ID_PREFIX,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};

// ── The predicate, without a database ────────────────────────────────────────

/// Live id shapes, asserted against the predicate directly.
///
/// `planning:48C93` is a real destination on `an_anglesey_aebaa785` — the one phase 45 design decision D-03
/// cites, reached from a planning-applicant director node. The `CF:` and `GB-COH:` shapes are the other two
/// that actually occur.
#[test]
fn only_the_planning_prefix_marks_a_planning_application() {
    assert!(
        is_planning_application_id("planning:48C93"),
        "`planning:` is the positive predicate — this is 86.9% of the corpus"
    );
    assert!(
        is_planning_application_id("planning:35C67A"),
        "a second real planning id, so the first is not passing on some accident of its suffix"
    );
    assert!(
        !is_planning_application_id("CF:ocds-b5fd17-4d1f2a"),
        "a Contracts Finder award is a contract"
    );
    assert!(
        !is_planning_application_id("GB-COH:99000001"),
        "a company id is not a planning application"
    );
}

/// **The assertion that catches the backwards predicate.**
///
/// If the split is ever rewritten as `starts_with("contract:")` — the framing the plan and the
/// context document both started from — this is the test that goes red. Every other assertion in
/// this file and in `profile_paging.rs` would go red too, but they would go red by reporting empty
/// sections, which reads like a fixture problem. This one names the cause.
///
/// The `contract:` prefix occurs in exactly two places in this workspace: `profile_paging.rs`'s
/// synthetic `contract:x9999-con-…` fixture ids and this test. It occurs **zero** times in the
/// store. A destination carrying it is a contract because it is *not* a planning application, which
/// is the only reason anything is a contract.
#[test]
fn a_contract_prefixed_destination_is_a_contract_not_a_planning_application() {
    assert!(
        !is_planning_application_id("contract:x9999-con-00001"),
        "`contract:` must classify as a CONTRACT. If this fails the predicate has been inverted \
         to `starts_with(\"contract:\")`, which matches zero rows in the live store and empties \
         the contracts section while leaving planning applications where they were."
    );
    assert_ne!(
        PLANNING_APPLICATION_ID_PREFIX, "contract:",
        "the prefix constant names what a planning application is, not what a contract is"
    );
    assert_eq!(PLANNING_APPLICATION_ID_PREFIX, "planning:");
}

/// The prefix is matched exactly, so a differently-cased id is not silently reclassified. Every
/// planning destination in the store is lowercase `planning:`; an id that is not is a different
/// node, not the same one shouting.
#[test]
fn the_prefix_match_is_case_sensitive() {
    assert!(!is_planning_application_id("Planning:48C93"));
    assert!(!is_planning_application_id("PLANNING:48C93"));
    assert!(
        !is_planning_application_id("preplanning:48C93"),
        "the prefix anchors at the start — a longer scheme that merely contains it is not a match"
    );
}

// ── The split, against a database ────────────────────────────────────────────

/// The subject. Synthetic `X9998…` numbering, never a live analysis id.
const SUBJECT: &str = "GB-COH:X9998001";

/// Contracts Finder awards, keyed the way the CF mapper keys them.
const N_CF: usize = 6;

/// Planning applications, keyed the way the planning mapper keys them.
const N_PLANNING: usize = 9;

/// The `contract:`-prefixed destination. Not a shape the store produces — it exists so the
/// backwards predicate has something to misclassify.
const TRAP_ID: &str = "contract:x9998-trap-00001";
const TRAP_NAME: &str = "X9998 Trap Contract";

/// A `planning:` destination whose stored weight carries a `tender_ref` and **no**
/// `application_ref` — the two signals disagreeing.
///
/// `is_planning_application_id` documents that the destination **id** is the predicate and the
/// payload refs are not, because the id is on the edge while the refs are on a node that may not
/// have been ingested. This row is where that choice is observable. The two signals agree on every
/// one of the 108,799 `Contract` node-weight events in the live store (measured 2026-09-15), so
/// without it the file would be consistent with either predicate.
const DISAGREEING_ID: &str = "planning:x9998-noref";
const DISAGREEING_NAME: &str = "X9998 Planning Application With No Application Ref";

fn cf_id(i: usize) -> String {
    format!("CF:x9998-cf-{i:05}")
}

fn cf_name(i: usize) -> String {
    format!("X9998 CF Award {i:05}")
}

fn planning_id(i: usize) -> String {
    format!("planning:x9998-app-{i:05}")
}

fn planning_name(i: usize) -> String {
    format!("X9998 Planning Application {i:05}")
}

fn base_timestamp() -> u64 {
    1_700_000_000_000
}

fn node_row(event_id: &str, entity_id: &str, weight: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": base_timestamp(),
        "kind": "NodeAdded",
        "payload": { "entity_id": entity_id, "weight_json": weight.to_string() },
    })
}

fn edge_row(event_id: &str, src_id: &str, dst_id: &str) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": base_timestamp(),
        "kind": "EdgeAdded",
        "payload": {
            "src_id": src_id,
            "dst_id": dst_id,
            "weight_json": serde_json::json!({
                "type": "AwardedTo",
                "meta": {
                    "created_at": "2024-01-01T00:00:00Z",
                    "effective_from": null,
                    "effective_to": null
                },
                "data": { "details": "seeded" },
            }).to_string(),
        },
    })
}

fn entity_weight(name: &str, company_number: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Entity",
        "name": name,
        "company_number": company_number,
        // EntityStatus is `#[serde(tag = "type", content = "content")]` — the wire form of Active
        // is an object, not the bare string a fixture is tempted to write.
        "status": { "type": "Active" },
    })
}

/// A Contracts Finder award, shaped as the CF mapper writes one: a value, a publication date and a
/// `tender_ref`, no `application_ref`.
fn cf_weight(name: &str, tender_ref: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Contract",
        "name": name,
        "value_gbp": 125_000.0,
        "status": "awarded",
        "date_published": "2024-03-01",
        "ward_code": "X05999001",
        "tender_ref": tender_ref,
    })
}

/// A planning application, shaped as the planning mapper writes one: no value, no publication date,
/// no tender reference — verified against the live store, where zero of 102,619 `planning:`
/// node-weight events serialise a `value_gbp` or a `tender_ref`.
fn planning_weight(name: &str, application_ref: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Contract",
        "name": name,
        "status": "pending",
        "ward_code": "X05999001",
        "application_ref": application_ref,
        "applicant_provenance": "published",
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

/// What the fixture expects back, by section. Names rather than ids: this file asserts the
/// *split*, and a name is the value the split was originally observable through. (D-06 has since
/// put an `id` on both row types — `crates/core/tests/profile_pivot_ids.rs` is where that is
/// asserted; this file deliberately keeps matching on names so a change to the id scheme cannot
/// quietly rewrite what "contracts and planning applications stay two sections" means.)
struct Expected {
    contract_names: BTreeSet<String>,
    planning_names: BTreeSet<String>,
}

async fn seed(db: &Surreal<Any>) -> Expected {
    let mut rows = vec![node_row(
        "x9998-subject",
        SUBJECT,
        entity_weight("X9998 Subject Ltd", "X9998001"),
    )];
    let mut contract_names = BTreeSet::new();
    let mut planning_names = BTreeSet::new();

    for i in 0..N_CF {
        let id = cf_id(i);
        rows.push(node_row(
            &format!("x9998-cf-node-{i}"),
            &id,
            cf_weight(&cf_name(i), &format!("X9998-TENDER-{i:05}")),
        ));
        rows.push(edge_row(&format!("x9998-cf-edge-{i}"), SUBJECT, &id));
        contract_names.insert(cf_name(i));
    }

    for i in 0..N_PLANNING {
        let id = planning_id(i);
        rows.push(node_row(
            &format!("x9998-app-node-{i}"),
            &id,
            planning_weight(&planning_name(i), &format!("X9998C{i:03}")),
        ));
        rows.push(edge_row(&format!("x9998-app-edge-{i}"), SUBJECT, &id));
        planning_names.insert(planning_name(i));
    }

    // The trap: a `contract:`-prefixed destination, which the store never produces. It belongs
    // under contracts, and it is the row that moves if the predicate is inverted.
    rows.push(node_row(
        "x9998-trap-node",
        TRAP_ID,
        cf_weight(TRAP_NAME, "X9998-TENDER-TRAP"),
    ));
    rows.push(edge_row("x9998-trap-edge", SUBJECT, TRAP_ID));
    contract_names.insert(TRAP_NAME.to_owned());

    // The disagreement: a `planning:` id whose weight looks like a CF award. The id wins.
    rows.push(node_row(
        "x9998-noref-node",
        DISAGREEING_ID,
        cf_weight(DISAGREEING_NAME, "X9998-TENDER-DISAGREE"),
    ));
    rows.push(edge_row("x9998-noref-edge", SUBJECT, DISAGREEING_ID));
    planning_names.insert(DISAGREEING_NAME.to_owned());

    insert_rows(db, rows).await;
    Expected {
        contract_names,
        planning_names,
    }
}

/// A `count()` issued against the database, independent of the seeder's own bookkeeping.
///
/// `count()` is the one aggregate the SurrealDB 3.x planner bug leaves correct, so it is the right
/// second opinion on "did the batched INSERT land". Without it, a partially-failed seed would have
/// every assertion below comparing the read path against a fiction.
async fn count_awarded_edges_from(db: &Surreal<Any>, src: &str) -> usize {
    let n: Option<i64> = db
        .query(
            "SELECT count() FROM graph_events \
             WHERE kind = 'EdgeAdded' AND payload.src_id = $src GROUP ALL",
        )
        .bind(("src", src.to_owned()))
        .await
        .expect("count query")
        .take((0, "count"))
        .expect("count take");
    n.unwrap_or(0) as usize
}

/// D-03: the profile returns planning applications as their own section, and the split is a
/// partition of the `AwardedTo` destinations rather than a filter that can drop one.
#[tokio::test]
async fn db_planning_applications_are_a_separate_section_from_contracts() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_planning_applications_are_a_separate_section_from_contracts \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    // `TestNs` derefs to `Surreal<Any>` and removes its namespace on drop, so the handle must
    // stay bound for the whole test — never a hand-rolled `format!("test_{uuid}")`, which is the
    // pattern that stranded 2,889 namespaces in the local store.
    let db = TestNs::connect("profile_sections_split").await;

    let expected = seed(&db).await;

    // ── Non-vacuity, before anything is compared ─────────────────────────────
    // Both sides of every comparison below must be known non-empty first. A version of this test
    // that compared two empty sets would pass while the split had emptied BOTH sections, which is
    // precisely the failure the backwards `contract:` predicate produces.
    let seeded_destinations = expected.contract_names.len() + expected.planning_names.len();
    assert!(
        !expected.contract_names.is_empty() && !expected.planning_names.is_empty(),
        "NON-VACUITY: the fixture must seed BOTH kinds — no live subject does, so this fixture is \
         the only place the two-section invariant is observable"
    );
    let counted = count_awarded_edges_from(&db, SUBJECT).await;
    assert_eq!(
        counted, seeded_destinations,
        "NON-VACUITY: the database's own count() must agree with the seeder's bookkeeping \
         ({counted} counted vs {seeded_destinations} seeded), or the batched INSERT partially \
         failed and everything below compares the read path against a fiction"
    );

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("query_entity_by_id failed")
        .expect("the seeded subject must be found");

    assert!(
        !profile.contracts.is_empty(),
        "NON-VACUITY: the contracts section came back empty. On a fixture seeding {N_CF} CF \
         awards plus a `contract:`-prefixed destination that means the split matched the wrong \
         prefix, not that the subject has no contracts."
    );
    assert!(
        !profile.planning_applications.is_empty(),
        "NON-VACUITY: the planning applications section came back empty on a fixture seeding \
         {N_PLANNING} of them"
    );

    // ── The split itself ─────────────────────────────────────────────────────
    let got_contracts: BTreeSet<String> =
        profile.contracts.iter().map(|c| c.name.clone()).collect();
    let got_planning: BTreeSet<String> = profile
        .planning_applications
        .iter()
        .map(|a| a.name.clone())
        .collect();

    assert_eq!(
        got_contracts, expected.contract_names,
        "the contracts section must hold exactly the non-`planning:` destinations"
    );
    assert_eq!(
        got_planning, expected.planning_names,
        "the planning applications section must hold exactly the `planning:` destinations"
    );

    // ── It is a partition, not two independent filters ───────────────────────
    assert!(
        got_contracts.is_disjoint(&got_planning),
        "no destination may appear in both sections"
    );
    assert_eq!(
        profile.contracts.len() + profile.planning_applications.len(),
        seeded_destinations,
        "every awarded destination must land in exactly one section — the two sections together \
         must still be everything the single section used to be"
    );

    // ── No planning reference under contracts (the plan's literal ask) ───────
    //
    // `ContractRow::application_ref` is deliberately still a field, so this is a claim about
    // behaviour rather than a tautology about types. Delete the field and this assertion becomes
    // unwritable and the invariant unguarded.
    let leaked: Vec<&str> = profile
        .contracts
        .iter()
        .filter_map(|c| c.application_ref.as_deref())
        .collect();
    assert!(
        leaked.is_empty(),
        "no row under contracts may carry a planning application reference, found {leaked:?}"
    );

    // ── The id is the predicate, not the payload ─────────────────────────────
    assert!(
        got_planning.contains(DISAGREEING_NAME),
        "a `planning:` destination is a planning application even when its stored weight carries \
         a tender_ref and no application_ref — the id is on the edge, the refs are on a node that \
         may not have been ingested"
    );
    assert!(
        got_contracts.contains(TRAP_NAME),
        "a `contract:`-prefixed destination is a CONTRACT. If this fails the predicate has been \
         inverted to `starts_with(\"contract:\")` — see this file's module docs for why that \
         reads as green everywhere else."
    );

    // ── The planning row shape ───────────────────────────────────────────────
    let sample = profile
        .planning_applications
        .iter()
        .find(|a| a.name == planning_name(0))
        .expect("the first seeded planning application must be present");
    assert_eq!(
        sample.application_ref.as_deref(),
        Some("X9998C000"),
        "the application reference must survive the move into its own section"
    );
    assert_eq!(sample.status.as_deref(), Some("pending"));
    assert_eq!(sample.ward_code.as_deref(), Some("X05999001"));
    assert_eq!(
        sample.published, None,
        "the planning mapper writes no date_published — asserted so that a future mapper change \
         that starts writing one is noticed here rather than in a report diff"
    );
}
