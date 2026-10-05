//! PIV-04: the "as at" filter, and the null-range rule that is the whole of it.
//!
//! # The failure this file exists to catch
//!
//! Almost nothing in this store carries a date range. Measured against `an_anglesey_aebaa785` on
//! 2026-09-15 over 986,062 `graph_events` rows:
//!
//! | | |
//! |---|---|
//! | `EdgeAdded` events | 511,754 |
//! | …carrying `effective_to: null` | **511,754 (100%)** |
//! | …carrying `effective_from: null` | 384,363 (75.1%) |
//! | edge types with any `effective_from` at all | **2 of 12** — `Controls` (19,587) and `OwnsProperty` (107,804) |
//! | `Directs` / `AwardedTo` / `LocatedAt` events with either endpoint | **0 of 248,420** |
//!
//! So an as-at filter meets an undated row far more often than a dated one, and the rule for that
//! case decides whether the feature works or silently destroys the page:
//!
//! * drop undated rows → Officers, Contracts, Planning applications and Addresses all empty at
//!   once, and the screen cannot tell the analyst whether the filter worked or the query broke;
//! * keep them unmarked → the filter is a no-op on those same four sections, with nothing saying
//!   so.
//!
//! Neither raises an error. Both look like a working feature. **A test that only exercises the
//! dated case passes against either of them**, which is why both arms below get their own test and
//! each is proved to fail under the inverted rule.
//!
//! The rule this workspace chose is
//! `urban_integrity_workbench_dto::temporal::NULL_RANGE_RULE`: an undated row is **retained and
//! labelled** `AsAtStatus::Undated`. It is stated to the analyst through that constant and through
//! `UNDATED_ROWS_ARE`, which the phase 45 UI spec's as-at banner renders verbatim.
//!
//! # PIV-04's worked example runs on a fixture, and cannot run on this corpus
//!
//! "A director who left before a contract was awarded is visibly distinguished from one who was
//! serving" has **no live data**, for two independent reasons:
//!
//! 1. 45-04 measured the Directors section empty for 99.8% of the corpus. Of 24,662 `Directs`
//!    events, 24,503 run `GB-COH:` → `director:` — the opposite direction from the one
//!    `assemble_entity_profile` reads — leaving 143 events across 132 distinct entities.
//! 2. **Zero of 24,485 `Officer` node-weight events carry a `resigned_on`.** 24,352 carry an
//!    `appointed_on`; none carries an end. So no officer in this store has ever resigned as far as
//!    the data says, and "a director who left" has no instance anywhere in the corpus — not merely
//!    none among the 132 entities that can show a directors row.
//!
//! (2) is the stronger reason and it is independent of (1): fixing the edge direction would
//! populate the Officers section for the whole corpus and still leave every row open-ended.
//!
//! **One correction to 45-04 while measuring this.** 45-04 characterised the 143 surviving rows as
//! "a company slug in a person column… the planning-identity join's company-resolution edges",
//! from the one example it inspected (`director:4-crosses-construction-ltd`, which has no `Officer`
//! node at all). That is true of 133 of them. The other **10 have real `Officer` nodes with real
//! `appointed_on` dates**, and 8 of those are `corporate-director`, `corporate-secretary` or
//! `corporate-llp-designated-member` appointments — Companies House genuinely records a company as
//! an officer of another company, so the slug looking like a company is correct rather than a
//! symptom. One of the two natural-person rows is a live director node,
//! and its profile renders `from: 2025-01-29, basis: officer.appointed_on / officer.resigned_on`
//! live today. So the as-at filter is operative on 10 director rows corpus-wide, all half-open.
//!
//! `db_the_worked_example_runs_on_a_seeded_fixture` therefore seeds its own subject and says so.
//! A green run of it does **not** mean an analyst can see this on `an_anglesey_aebaa785` today.
//! Fixing (1) is an edge-direction problem that moves Phoenix's traversal and wants its own plan;
//! (2) is an ingest gap in the CH officers backfill.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test profile_as_at
//! ```
//!
//! The pure-function tests need no database. The `db_`-prefixed ones are gated on
//! `URBAN_DB_TESTS=1` and skip by returning, so a gated run reports the same passing count as an
//! ungated one and duration is the only witness a database was reached. Namespaces come from
//! `urban-integrity-test-support`, which drops them.

use chrono::NaiveDate;
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    apply_as_at, as_at_status, effective_range, iso_date, query_entity_by_id_as_at, AsAtStatus,
    EffectiveRange, NULL_RANGE_RULE, UNDATED_ROWS_ARE,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("a test date")
}

fn recorded(from: Option<&str>, to: Option<&str>) -> EffectiveRange {
    EffectiveRange::Recorded {
        from: from.map(str::to_owned),
        to: to.map(str::to_owned),
        basis: "test".to_owned(),
    }
}

fn unrecorded() -> EffectiveRange {
    EffectiveRange::Unrecorded {
        basis: "test".to_owned(),
    }
}

// ── Arm one: an undated row is RETAINED, and says it was not tested ──────────

/// **The first arm of the null case.** A range with no endpoints is `Undated`, never `OutOfForce`
/// and never `InForce`.
///
/// `Undated` is what keeps the row on the profile: `apply_as_at` removes nothing, and a renderer
/// that hides `OutOfForce` rows (45-12) must not hide these. Were this to return `OutOfForce`, the
/// 248,420 `Directs`/`AwardedTo`/`LocatedAt`-derived rows in this store would every one of them
/// fall behind the "outside as at" treatment the moment an analyst set a date.
///
/// PROVE THIS CAN FAIL: in `as_at_status`, replace the two `return AsAtStatus::Undated` arms with
/// `AsAtStatus::OutOfForce`. This test fails on the first assertion, naming the rule.
#[test]
fn an_undated_row_is_undated_not_excluded_and_not_asserted_in_force() {
    for as_at in ["1900-01-01", "2020-06-15", "2999-12-31"] {
        assert_eq!(
            as_at_status(&unrecorded(), day(as_at)),
            AsAtStatus::Undated,
            "an unrecorded range must be Undated at every date, including {as_at}. \
             OutOfForce would hide every Directs-, AwardedTo- and LocatedAt-derived row on this \
             corpus (248,420 events, none carrying an endpoint); InForce would claim something no \
             dataset said."
        );
    }

    // A `Recorded` range whose endpoints are present but unreadable is the same answer. The record
    // said something; nothing in it reads as a date; the filter has therefore not tested this row.
    assert_eq!(
        as_at_status(&recorded(Some("not a date"), Some("")), day("2020-06-15")),
        AsAtStatus::Undated,
        "an endpoint that cannot be parsed leaves the row untested, not in force"
    );
}

/// **The second arm.** A row the record positively places outside the date is `OutOfForce` — the
/// filter is not a no-op, and the distinction it draws is real.
///
/// Three shapes, because a half-open range is the normal case here (`effective_to` is null on all
/// 511,754 edge events, so the only endpoint that can ever exclude a row on today's corpus is the
/// start).
///
/// PROVE THIS CAN FAIL: in `as_at_status`, delete the
/// `if from.is_some_and(|f| f > as_at) || to.is_some_and(|t| t < as_at)` arm so every readable
/// range returns `InForce`. This test fails on the first assertion.
#[test]
fn a_row_the_record_places_outside_the_date_is_out_of_force() {
    let at = day("2020-06-15");

    // Began after the date.
    assert_eq!(
        as_at_status(&recorded(Some("2021-01-01"), None), at),
        AsAtStatus::OutOfForce
    );
    // Ended before it.
    assert_eq!(
        as_at_status(&recorded(None, Some("2019-12-31")), at),
        AsAtStatus::OutOfForce
    );
    // A closed range entirely in the past.
    assert_eq!(
        as_at_status(&recorded(Some("2015-01-04"), Some("2019-12-31")), at),
        AsAtStatus::OutOfForce
    );

    // …and the complements, so this test is about a distinction rather than a constant.
    assert_eq!(
        as_at_status(&recorded(Some("2015-01-04"), None), at),
        AsAtStatus::InForce
    );
    assert_eq!(
        as_at_status(&recorded(None, Some("2021-01-01")), at),
        AsAtStatus::InForce
    );
    assert_eq!(
        as_at_status(&recorded(Some("2015-01-04"), Some("2021-01-01")), at),
        AsAtStatus::InForce
    );
}

/// The endpoints are inclusive, and a half-open range is in force rather than undated.
///
/// "Began on the as-at date" and "ended on it" are both in force: a directorship that started on
/// the day is a directorship on the day. And a range with a readable start and no recorded end is
/// **tested**, not skipped — that is the shape of every `owns_property` and `controls` row in this
/// store (107,804 and 19,547 respectively, none carrying an end), so treating it as untestable
/// would undate the only two sections the filter can act on.
#[test]
fn the_endpoints_are_inclusive_and_a_half_open_range_is_still_tested() {
    let at = day("2020-06-15");
    assert_eq!(
        as_at_status(&recorded(Some("2020-06-15"), None), at),
        AsAtStatus::InForce
    );
    assert_eq!(
        as_at_status(&recorded(None, Some("2020-06-15")), at),
        AsAtStatus::InForce
    );
    assert_eq!(
        as_at_status(&recorded(Some("2020-06-15"), Some("2020-06-15")), at),
        AsAtStatus::InForce
    );
}

/// Both date shapes this store produces parse to the same day. `OfficerData::appointed_on` and
/// `ContractData::date_published` are bare `YYYY-MM-DD` from their mappers; `EdgeMeta` endpoints
/// are chrono `DateTime<Utc>` serialised as RFC-3339 with nanosecond precision. A reader that only
/// handled one of them would silently undate the other.
#[test]
fn a_bare_date_and_an_rfc_3339_datetime_read_as_the_same_day() {
    assert_eq!(iso_date("2016-04-06"), Some(day("2016-04-06")));
    assert_eq!(iso_date("2016-04-06T00:00:00Z"), Some(day("2016-04-06")));
    assert_eq!(
        iso_date("2026-07-25T14:41:33.327466130Z"),
        Some(day("2026-07-25")),
        "the shape `owns_property.meta.created_at` actually carries in this store"
    );
    assert_eq!(
        iso_date("2026-07-01T08:51:18+01:00"),
        Some(day("2026-07-01")),
        "an offset datetime — the shape `ContractData::date_published` actually carries for some \
         CF awards on the live corpus. The offset is deliberately NOT normalised to UTC: the local \
         date is the publication date an analyst would cite."
    );
    assert_eq!(iso_date(""), None);
    assert_eq!(iso_date("2016-04"), None);
    assert_eq!(
        iso_date("06/04/2016"),
        None,
        "a UK display date is not an ISO date"
    );

    // A multi-byte prefix must not panic on a byte slice — the resolver in this workspace has
    // already crashed once on exactly that class of bug (rphonetic, 2026-06-09).
    assert_eq!(iso_date("café"), None);
}

/// A row takes its range from the first candidate that states one, and the winning candidate's
/// label becomes the basis — so a reader can tell which record answered.
///
/// This is the officers case concretely: the Officer node's tenure wins when it exists, and the
/// `Directs` edge's meta is the fallback. When neither answers, the basis names **both**, which is
/// the difference between "this dataset records no dates" and "this row lost its dates".
#[test]
fn the_range_names_the_record_field_it_was_read_from() {
    let officer = "officer.appointed_on / officer.resigned_on";
    let edge = "directs.meta.effective_from / directs.meta.effective_to";

    let from_officer = effective_range(&[
        (officer, Some("2015-01-04".to_owned()), None),
        (edge, Some("1999-01-01".to_owned()), None),
    ]);
    let EffectiveRange::Recorded { from, basis, .. } = &from_officer else {
        panic!("expected Recorded, got {from_officer:?}");
    };
    assert_eq!(
        from.as_deref(),
        Some("2015-01-04"),
        "the officer tenure wins"
    );
    assert_eq!(basis, officer);

    let from_edge = effective_range(&[
        (officer, None, None),
        (edge, None, Some("2021-09-30".to_owned())),
    ]);
    let EffectiveRange::Recorded { to, basis, .. } = &from_edge else {
        panic!("expected Recorded, got {from_edge:?}");
    };
    assert_eq!(to.as_deref(), Some("2021-09-30"));
    assert_eq!(basis, edge, "the fallback answered, and the basis says so");

    let neither = effective_range(&[(officer, None, None), (edge, None, None)]);
    let EffectiveRange::Unrecorded { basis } = &neither else {
        panic!("expected Unrecorded, got {neither:?}");
    };
    assert!(
        basis.contains(officer) && basis.contains(edge),
        "an unrecorded range must name EVERY field consulted, not just the last: {basis}"
    );
}

/// The rule the code applies and the sentence the analyst reads are the same decision.
///
/// The phase 45 UI spec fixes the as-at banner's bracketed word "at build time by plan 45-06's null-range
/// decision". This asserts the word and the behaviour agree, so flipping one without the other
/// fails here rather than on screen.
///
/// PROVE THIS CAN FAIL: set `UNDATED_ROWS_ARE` to `"excluded"` — the second assertion fails. Or
/// invert `as_at_status`'s undated arm to `OutOfForce` — the third fails.
#[test]
fn the_rule_on_the_screen_is_the_rule_in_the_code() {
    assert!(
        NULL_RANGE_RULE.contains(UNDATED_ROWS_ARE),
        "the banner word must appear in the sentence that explains it"
    );
    assert_eq!(
        UNDATED_ROWS_ARE, "included",
        "the screen says undated facts are INCLUDED"
    );
    assert_ne!(
        as_at_status(&unrecorded(), day("2020-06-15")),
        AsAtStatus::OutOfForce,
        "…so the code must not exclude them"
    );
}

// ── The seeded fixture: PIV-04's worked example ──────────────────────────────

const SUBJECT: &str = "GB-COH:X9997001";
const SERVING: &str = "director:x9997-still-serving";
const RESIGNED: &str = "director:x9997-already-resigned";
const UNDATED_OFFICER: &str = "director:x9997-no-tenure-recorded";
const AWARD_ID: &str = "CF:x9997-award-00001";
const AWARD_DATE: &str = "2022-03-01";
const UNDATED_AWARD_ID: &str = "planning:x9997-app-00001";

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

/// An `EdgeAdded` row whose `meta` endpoints are given explicitly, so a fixture can seed a dated
/// edge as well as the undated shape the whole store actually holds.
fn edge_row(
    event_id: &str,
    src_id: &str,
    dst_id: &str,
    edge_type: &str,
    effective_from: Option<&str>,
    effective_to: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": base_timestamp(),
        "kind": "EdgeAdded",
        "payload": {
            "src_id": src_id,
            "dst_id": dst_id,
            "weight_json": serde_json::json!({
                "type": edge_type,
                "meta": {
                    "created_at": "2024-01-01T00:00:00Z",
                    "effective_from": effective_from,
                    "effective_to": effective_to,
                },
                "data": { "details": "seeded" },
            }).to_string(),
        },
    })
}

fn entity_weight(name: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Entity",
        "name": name,
        "company_number": "X9997001",
        "status": { "type": "Active" },
    })
}

/// An Officer node. `resigned_on` is the field **no live row in this store carries** — 0 of 24,485
/// `Officer` events serialise one — which is the second reason PIV-04's example cannot be shown on
/// the live corpus and the reason this fixture exists.
fn officer_weight(name: &str, appointed: &str, resigned: Option<&str>) -> serde_json::Value {
    let mut w = serde_json::json!({
        "type": "Officer",
        "name": name,
        "officer_role": "director",
        "appointed_on": appointed,
    });
    if let Some(r) = resigned {
        w["resigned_on"] = serde_json::Value::String(r.to_owned());
    }
    w
}

fn cf_weight(name: &str, date_published: Option<&str>) -> serde_json::Value {
    let mut w = serde_json::json!({
        "type": "Contract",
        "name": name,
        "value_gbp": 125_000.0,
        "status": "awarded",
        "ward_code": "X05999001",
        "tender_ref": "X9997-TENDER-00001",
    });
    if let Some(d) = date_published {
        w["date_published"] = serde_json::Value::String(d.to_owned());
    }
    w
}

fn planning_weight(name: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "Contract",
        "name": name,
        "status": "pending",
        "ward_code": "X05999001",
        "application_ref": "X9997C001",
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

/// A `count()` issued against the database, independent of the seeder's own bookkeeping.
///
/// `count()` is the one aggregate the SurrealDB 3.x planner bug leaves correct, so it is the right
/// second opinion on "did the batched INSERT land". Without it a partially-failed seed would have
/// every assertion below comparing the read path against a fiction.
async fn count_events(db: &Surreal<Any>) -> usize {
    let n: Option<i64> = db
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .expect("count query")
        .take((0, "count"))
        .expect("count take");
    n.unwrap_or(0) as usize
}

/// Three officers and two awards, chosen so every arm of the rule is observable on one subject:
///
/// | row | range | at 2022-03-01 |
/// |---|---|---|
/// | `SERVING` | 2018-04-02 → (none) | `InForce` |
/// | `RESIGNED` | 2016-01-05 → 2021-09-30 | `OutOfForce` |
/// | `UNDATED_OFFICER` | none on the node, none on the edge | `Undated` |
/// | `AWARD_ID` | published 2022-03-01 | `InForce` |
/// | `UNDATED_AWARD_ID` | no `date_published` | `Undated` |
///
/// Seeded rather than found, because no live subject has any of it (see the module doc).
async fn seed(db: &Surreal<Any>) -> usize {
    let rows = vec![
        node_row("x9997-subject", SUBJECT, entity_weight("X9997 Subject Ltd")),
        node_row(
            "x9997-off-serving",
            SERVING,
            officer_weight("X9997 Serving Director", "2018-04-02", None),
        ),
        node_row(
            "x9997-off-resigned",
            RESIGNED,
            officer_weight("X9997 Resigned Director", "2016-01-05", Some("2021-09-30")),
        ),
        node_row(
            "x9997-off-undated",
            UNDATED_OFFICER,
            // No `appointed_on` and no `resigned_on`: nothing states this directorship's dates.
            serde_json::json!({
                "type": "Officer",
                "name": "X9997 Undated Director",
                "officer_role": "director",
            }),
        ),
        // Every Directs edge in the live store is undated, so all three are seeded that way.
        edge_row("x9997-e-serving", SERVING, SUBJECT, "Directs", None, None),
        edge_row("x9997-e-resigned", RESIGNED, SUBJECT, "Directs", None, None),
        edge_row(
            "x9997-e-undated",
            UNDATED_OFFICER,
            SUBJECT,
            "Directs",
            None,
            None,
        ),
        node_row(
            "x9997-award-node",
            AWARD_ID,
            cf_weight("X9997 Highway Resurfacing", Some(AWARD_DATE)),
        ),
        edge_row(
            "x9997-award-edge",
            SUBJECT,
            AWARD_ID,
            "AwardedTo",
            None,
            None,
        ),
        node_row(
            "x9997-app-node",
            UNDATED_AWARD_ID,
            planning_weight("X9997 Change of Use"),
        ),
        edge_row(
            "x9997-app-edge",
            SUBJECT,
            UNDATED_AWARD_ID,
            "AwardedTo",
            None,
            None,
        ),
    ];
    let n = rows.len();
    insert_rows(db, rows).await;
    n
}

/// **PIV-04's worked example, on a seeded fixture.**
///
/// "A director who left before a contract was awarded is visibly distinguished from one who was
/// serving." Both directors are on the profile; the award's own date is on the profile; and the
/// difference between them is a field the reader looks at rather than a subtraction the reader
/// performs.
///
/// **This does not run on `an_anglesey_aebaa785` and cannot today** — see the module doc for the
/// two independent reasons. A green result here is a statement about the code, not about what an
/// analyst can see.
///
/// PROVE THIS CAN FAIL: in `assemble_entity_profile`, build the directors' range from
/// `edge_meta_candidate("directs", …)` alone (drop the officer-tenure candidate). Every officer
/// row becomes `Undated`, and the `RESIGNED`/`SERVING` distinction assertion fails by name.
#[tokio::test]
async fn db_the_worked_example_runs_on_a_seeded_fixture() {
    if !db_tests_enabled() {
        eprintln!("SKIP: db_the_worked_example_runs_on_a_seeded_fixture (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = TestNs::connect("profile_as_at_worked_example").await;
    let seeded = seed(&db).await;
    assert_eq!(
        count_events(&db).await,
        seeded,
        "NON-VACUITY: the database's own count() must agree with the seeder's bookkeeping, or \
         everything below compares the read path against a fiction"
    );

    let profile = query_entity_by_id_as_at(&db, SUBJECT, Some(day(AWARD_DATE)))
        .await
        .expect("query_entity_by_id_as_at failed")
        .expect("the seeded subject must be found");

    assert_eq!(
        profile.as_at,
        Some(day(AWARD_DATE)),
        "a filtered profile must say so; a reader who cannot tell it from an unfiltered one will \
         misread the page"
    );
    assert_eq!(
        profile.directors.len(),
        3,
        "NON-VACUITY: all three seeded officers must be on the profile. The filter MARKS and \
         removes nothing — a shorter list here means rows were dropped, which is the rule this \
         plan rejected. Got: {:?}",
        profile
            .directors
            .iter()
            .map(|d| &d.name)
            .collect::<Vec<_>>()
    );

    let by_name = |needle: &str| {
        profile
            .directors
            .iter()
            .find(|d| d.name.contains(needle))
            .unwrap_or_else(|| panic!("no seeded officer matching `{needle}`"))
    };
    let serving = by_name("Serving");
    let resigned = by_name("Resigned");
    let undated = by_name("Undated");

    // The distinction PIV-04 names, read off the rows rather than computed by the reader.
    assert_eq!(
        serving.temporal.status,
        AsAtStatus::InForce,
        "a director appointed 2018-04-02 with no recorded resignation was serving on {AWARD_DATE}"
    );
    assert_eq!(
        resigned.temporal.status,
        AsAtStatus::OutOfForce,
        "a director who resigned 2021-09-30 had left before the {AWARD_DATE} award"
    );
    assert_ne!(
        serving.temporal.status, resigned.temporal.status,
        "if these agree the filter is not drawing the distinction PIV-04 exists for"
    );
    assert_eq!(
        undated.temporal.status,
        AsAtStatus::Undated,
        "an officer whose tenure nothing records is retained and labelled, not silently judged"
    );

    // The dates themselves are on the page, so the distinction is legible without the status too.
    assert_eq!(resigned.resigned.as_deref(), Some("2021-09-30"));
    assert_eq!(serving.resigned, None);

    // The award carries its own date, which is the other half of the comparison.
    assert_eq!(
        profile.contracts.len(),
        1,
        "NON-VACUITY: the CF award must be on the profile"
    );
    let award = &profile.contracts[0];
    assert_eq!(award.published.as_deref(), Some(AWARD_DATE));
    assert_eq!(award.temporal.status, AsAtStatus::InForce);
}

/// **Both arms of the null case, through the whole read path rather than the pure predicate.**
///
/// The predicate tests above prove the rule; this proves the rule reaches the profile. A producer
/// that never populated `temporal` at all, or populated it from a constant, passes every
/// pure-function test in this file and fails here.
///
/// PROVE THIS CAN FAIL: replace `award_range`'s body with
/// `EffectiveRange::Unrecorded { basis: "…".into() }`. The dated-award assertion fails. Replace
/// `as_at_status`'s undated arm with `OutOfForce` and the undated-count assertion fails.
#[tokio::test]
async fn db_both_arms_of_the_null_case_survive_the_read_path() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_both_arms_of_the_null_case_survive_the_read_path (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_as_at_null_arms").await;
    let seeded = seed(&db).await;
    assert_eq!(
        count_events(&db).await,
        seeded,
        "NON-VACUITY: the seed landed whole"
    );

    let profile = query_entity_by_id_as_at(&db, SUBJECT, Some(day(AWARD_DATE)))
        .await
        .expect("query failed")
        .expect("subject found");

    // Count both populations BEFORE comparing anything. A version of this test that compared two
    // empty sets would pass while `temporal` was never populated at all.
    let statuses: Vec<AsAtStatus> = profile
        .directors
        .iter()
        .map(|d| d.temporal.status)
        .chain(profile.contracts.iter().map(|c| c.temporal.status))
        .chain(
            profile
                .planning_applications
                .iter()
                .map(|a| a.temporal.status),
        )
        .collect();
    assert_eq!(
        statuses.len(),
        5,
        "NON-VACUITY: the fixture seeds 3 officers, 1 award and 1 planning application; a \
         different count means rows were dropped or never assembled"
    );

    let dated = statuses
        .iter()
        .filter(|s| matches!(s, AsAtStatus::InForce | AsAtStatus::OutOfForce))
        .count();
    let undated = statuses
        .iter()
        .filter(|s| **s == AsAtStatus::Undated)
        .count();
    assert!(
        dated > 0,
        "NON-VACUITY: no row on this profile was testable, so the filter proved nothing. \
         Expected the CF award ({AWARD_DATE}) and two dated officers."
    );
    assert!(
        undated > 0,
        "NON-VACUITY: no row on this profile was undated, so the arm that governs 248,420 of this \
         store's edges is untested by this fixture."
    );
    assert_eq!(
        dated + undated,
        statuses.len(),
        "no row may be left unclassified"
    );

    // Arm one, through the read path: the undated rows are STILL HERE.
    assert_eq!(
        undated, 2,
        "the officer with no recorded tenure and the planning application with no date_published \
         must both be retained and labelled Undated, not dropped. Statuses: {statuses:?}"
    );
    let undated_award = profile
        .planning_applications
        .first()
        .expect("the undated planning application must be retained under NULL_RANGE_RULE");
    assert_eq!(undated_award.temporal.status, AsAtStatus::Undated);
    assert!(
        undated_award.temporal.range.is_unrecorded(),
        "…and it must say its range is unrecorded rather than carrying an invented one"
    );

    // Arm two: the dated rows were actually tested, and the test discriminated.
    assert!(
        statuses.contains(&AsAtStatus::InForce) && statuses.contains(&AsAtStatus::OutOfForce),
        "a filter that returned one constant for every dated row would pass an assertion that \
         only checked `dated > 0`. Statuses: {statuses:?}"
    );
}

/// An unfiltered profile makes no claim about any date — `NotFiltered`, not `InForce`.
///
/// The two are different answers and conflating them is a false reassurance: a profile nobody
/// filtered has not established that anything was in force at any moment. The ranges are on the
/// rows either way, which is the phase 45 UI spec's "the unfiltered profile must not be the less
/// informative screen".
///
/// PROVE THIS CAN FAIL: make `Temporal::unfiltered` construct `AsAtStatus::InForce`. The status
/// assertion fails; the range assertions still pass, which is exactly why the status is asserted
/// separately.
#[tokio::test]
async fn db_an_unfiltered_profile_states_ranges_but_claims_no_status() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_an_unfiltered_profile_states_ranges_but_claims_no_status \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_as_at_unfiltered").await;
    let seeded = seed(&db).await;
    assert_eq!(
        count_events(&db).await,
        seeded,
        "NON-VACUITY: the seed landed whole"
    );

    let profile = query_entity_by_id_as_at(&db, SUBJECT, None)
        .await
        .expect("query failed")
        .expect("subject found");

    assert_eq!(profile.as_at, None);
    assert_eq!(
        profile.directors.len(),
        3,
        "NON-VACUITY: the officers are here to be checked"
    );
    for d in &profile.directors {
        assert_eq!(
            d.temporal.status,
            AsAtStatus::NotFiltered,
            "`{}` claims a filter verdict on an unfiltered profile",
            d.name
        );
    }

    // The ranges are still stated — that is the point.
    let resigned = profile
        .directors
        .iter()
        .find(|d| d.name.contains("Resigned"))
        .expect("the resigned officer");
    let EffectiveRange::Recorded { from, to, basis } = &resigned.temporal.range else {
        panic!(
            "the resigned officer's range must be Recorded: {:?}",
            resigned.temporal.range
        );
    };
    assert_eq!(from.as_deref(), Some("2016-01-05"));
    assert_eq!(to.as_deref(), Some("2021-09-30"));
    assert_eq!(
        basis, "officer.appointed_on / officer.resigned_on",
        "the range must name the record it was read from; the `Directs` edge beneath it is undated"
    );
}

/// `apply_as_at` is idempotent in the only sense that matters: applying a second, different date
/// re-evaluates every row rather than leaving the first verdict in place.
///
/// A renderer changes the as-at date repeatedly. A stamp that only wrote when the status was
/// `NotFiltered` would leave the first date's answers on screen under the second date's banner —
/// the exact "filter that silently changes the population without saying so" 45-12's objective
/// names.
#[tokio::test]
async fn db_changing_the_as_at_date_re_evaluates_every_row() {
    if !db_tests_enabled() {
        eprintln!("SKIP: db_changing_the_as_at_date_re_evaluates_every_row (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = TestNs::connect("profile_as_at_reapply").await;
    let seeded = seed(&db).await;
    assert_eq!(
        count_events(&db).await,
        seeded,
        "NON-VACUITY: the seed landed whole"
    );

    let mut profile = query_entity_by_id_as_at(&db, SUBJECT, Some(day(AWARD_DATE)))
        .await
        .expect("query failed")
        .expect("subject found");

    let status_of = |p: &urban_integrity_core::EntityProfile, needle: &str| {
        p.directors
            .iter()
            .find(|d| d.name.contains(needle))
            .unwrap_or_else(|| panic!("no officer matching `{needle}`"))
            .temporal
            .status
    };

    assert_eq!(status_of(&profile, "Resigned"), AsAtStatus::OutOfForce);
    assert_eq!(status_of(&profile, "Serving"), AsAtStatus::InForce);

    // Roll back to a date inside the resigned director's tenure and before the other's
    // appointment: both verdicts must flip.
    apply_as_at(&mut profile, day("2017-06-01"));
    assert_eq!(profile.as_at, Some(day("2017-06-01")));
    assert_eq!(
        status_of(&profile, "Resigned"),
        AsAtStatus::InForce,
        "on 2017-06-01 the 2016–2021 director WAS serving; a stale verdict here means the second \
         apply_as_at did not re-evaluate"
    );
    assert_eq!(
        status_of(&profile, "Serving"),
        AsAtStatus::OutOfForce,
        "…and the 2018-appointed director had not yet been appointed"
    );
    assert_eq!(
        status_of(&profile, "Undated"),
        AsAtStatus::Undated,
        "the undated row is undated at every date — it is not a verdict that can change"
    );
}
