//! Router-level tests for every route plan 45-08 adds (PIV-01, PIV-02, PIV-05, CNV-01).
//!
//! # Through the router, never the function beneath it
//!
//! Plan 44-18's `router_round_trip_shortest_path_equals_dispatch` is the precedent and the reason:
//! a test that calls a handler function directly proves the function works and says **nothing**
//! about whether the route is wired, whether the path is spelled the way the browser spells it,
//! whether the method matches, or whether the response serialises. Four wave-4 plans are written
//! against this route table, and every one of those four failures would reach them as a blank
//! screen.
//!
//! So every test here goes through `boot::build_router` — the same function `boot()` serves — over
//! a real TCP socket, with a real HTTP client.
//!
//! # Two layers, and what each one can prove
//!
//! * [`every_route_reaches_its_handler`] is **ungated** and proves registration. Each request is
//!   shaped to fail inside its handler *before* any database call (a missing `id`, a blank name),
//!   so the status distinguishes three outcomes that all look alike from the browser: the route is
//!   not registered (404), the method is not registered on it (405), or the handler ran (anything
//!   else).
//! * The `db_*` tests are gated behind `URBAN_DB_TESTS=1` and prove the round trip: seeded rows in,
//!   the same rows out through HTTP, in the DTO shapes wave 4 renders.
//!
//! Namespaces come from `urban-integrity-test-support` (docs/TESTING.md's mandatory rule); the guard is
//! bound for the whole test, because dropping it removes the namespace.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_core::persistence::queries::{
    findings_naming_entity, AddressRow, ContractRow, EntityProfile, OsintLink,
    PlanningApplicationRow, ProbeCoverage, RelatedEntity, SharedConnection,
};
use urban_integrity_schema::types::edge::{AwardedToData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};
use urban_integrity_test_support::{db_tests_enabled, TestNs};
use urban_integrity_workbench::boot::WorkbenchState;
use urban_integrity_workbench::entity::profile_map::{to_dto, SectionWindow};
use urban_integrity_workbench::graph::fixture::workbench_from_graph_with_db;
use urban_integrity_workbench::graph::label_index::LabelIndex;
use urban_integrity_workbench_dto::investigation::{
    InvestigationDetailView, InvestigationSummaryView, PinOriginView, PinTargetView,
    SavedInvestigationView, SavedPinView, UnpinResultView,
};
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;
use urban_integrity_workbench_dto::provenance::Provenance;
use urban_integrity_workbench_dto::temporal::{AsAtStatus, EffectiveRange, Temporal};
use urban_integrity_workbench_dto::{DisambiguationView, EntityProfileDto};

// ── Fixture ──────────────────────────────────────────────────────────────────

/// Synthetic ids only — `scripts/check-no-analysis-literals.sh` forbids an analysis-specific
/// literal anywhere in this crate, and these must not collide with the live corpus either.
const SUBJECT: &str = "GB-COH:X06999001";
const SUBJECT_NAME: &str = "X9999 Example Holdings Ltd";
const SUBJECT_CH: &str = "X06999001";
const WARD_CODE: &str = "X05999001";
/// The three D-06 pivot destinations, in the id namespaces the live corpus actually uses:
/// `CF:` for a Contracts Finder award, `planning:` for an application, `location:` for an address.
const CONTRACT_ID: &str = "CF:x9999-cf-00001";
const PLANNING_ID: &str = "planning:x9999-app-00001";
const ADDRESS_ID: &str = "location:1-x9999-street,-llangefni";
const CONTRACTS: usize = 12;

async fn spawn_router(
    handle: &Arc<GraphCoordinatorHandle>,
    state: &Arc<WorkbenchState>,
) -> std::net::SocketAddr {
    let router =
        urban_integrity_workbench::boot::build_router(Arc::clone(handle), Arc::clone(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// A router over an EMPTY graph and an UNINITIALISED database handle — enough to prove routing,
/// never enough to prove a read.
async fn no_db_router() -> std::net::SocketAddr {
    let (handle, state) =
        urban_integrity_workbench::graph::fixture::workbench_from_graph(&[], &[], &[]).await;
    spawn_router(&handle, &state).await
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

fn contract_id(i: usize) -> String {
    format!("CF:x9999-cf-{i:05}")
}

/// Seed one entity with [`CONTRACTS`] Contracts Finder awards, fail-loud.
///
/// The seeded count is cross-checked against the database's own `count()` before any assertion
/// reads it — a partially-failed batch would otherwise have every test below comparing the read
/// path against a fiction (45-03's rule, and `count()` is the one aggregate the SurrealDB 3.x
/// planner bug leaves correct).
async fn seed(db: &Surreal<Any>) {
    let mut rows = vec![node_row(
        "x9999-subject",
        SUBJECT,
        serde_json::json!({
            "type": "Entity",
            "name": SUBJECT_NAME,
            "company_number": SUBJECT_CH,
            "status": { "type": "Active" },
        }),
    )];
    for i in 0..CONTRACTS {
        rows.push(node_row(
            &format!("x9999-c{i:05}"),
            &contract_id(i),
            serde_json::json!({
                "type": "Contract",
                "name": format!("X9999 CF Award {i:05}"),
                "value_gbp": 125_000.0,
                "status": "awarded",
                "date_published": "2024-03-01",
                "ward_code": WARD_CODE,
                "tender_ref": format!("X9999-TENDER-{i:05}"),
            }),
        ));
        rows.push(edge_row(
            &format!("x9999-e{i:05}"),
            SUBJECT,
            &contract_id(i),
        ));
    }

    db.query("INSERT INTO graph_events $rows")
        .bind(("rows", serde_json::Value::Array(rows)))
        .await
        .expect("seed: query")
        .check()
        .expect("seed: statement");

    let counted: Option<usize> = db
        .query("SELECT count() FROM graph_events WHERE kind = 'EdgeAdded' GROUP ALL")
        .await
        .expect("count: query")
        .check()
        .expect("count: statement")
        .take((0, "count"))
        .expect("count: take");
    assert_eq!(
        counted,
        Some(CONTRACTS),
        "the batched INSERT did not land in full — every assertion below would be comparing the \
         read path against a fiction"
    );
}

/// A router over a real, seeded database.
async fn seeded_router(db_name: &str) -> (TestNs, std::net::SocketAddr) {
    let ns = TestNs::connect(db_name).await;
    seed(&ns).await;
    let (handle, state) = workbench_from_graph_with_db(&[], &[], &[], ns.arc()).await;
    let addr = spawn_router(&handle, &state).await;
    (ns, addr)
}

// ── Registration: ungated, no database ───────────────────────────────────────

/// **Every route this plan adds is registered, at the path and method the browser will use.**
///
/// Each request below is shaped to fail inside its handler before any database call, so a handler
/// that ran is distinguishable from a route that is not there. 404 means the path is unregistered;
/// 405 means the method is not registered on a path that is.
///
/// PROVE THIS CAN FAIL: delete any one `.route(...)` for these paths from
/// `boot::build_router` and this test fails naming that route — the mutation was run for
/// `/api/entity/neighbourhood` (404 NOT_FOUND) and for the `.get(...)` half of
/// `/api/investigations` (405 METHOD_NOT_ALLOWED, which is the case a path-only check would have
/// missed).
#[tokio::test]
async fn every_route_reaches_its_handler() {
    let addr = no_db_router().await;
    let client = reqwest::Client::builder()
        // A route that is NOT registered answers instantly. A handler that reached the
        // uninitialised database handle may never answer at all — see the `GET /api/investigations`
        // case below, where the timeout is itself the evidence.
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    let pin_target = serde_json::json!({ "kind": "entity", "entity_id": "GB-COH:X06999001" });
    let cases: Vec<(&str, &str, Option<serde_json::Value>)> = vec![
        // `id` missing -> 400 from the handler's own parameter check.
        ("GET", "/api/entity/profile", None),
        // An empty query is 200 with an empty list: "No entity matches" is not a failure state.
        ("GET", "/api/entity/disambiguate", None),
        ("GET", "/api/entity/neighbourhood", None),
        // A blank name is rejected by `create_investigation`'s validation before the write.
        (
            "POST",
            "/api/investigations",
            Some(serde_json::json!({ "name": "", "notes": "" })),
        ),
        ("GET", "/api/investigation", None),
        (
            "PATCH",
            "/api/investigation",
            Some(serde_json::json!({ "name": "x", "notes": null })),
        ),
        ("DELETE", "/api/investigation", None),
        (
            "POST",
            "/api/investigation/pin",
            Some(serde_json::json!({ "target": pin_target, "origin": "profile", "note": null })),
        ),
        (
            "POST",
            "/api/investigation/unpin",
            Some(serde_json::json!({ "target": pin_target })),
        ),
        (
            "PATCH",
            "/api/investigation/pin",
            Some(serde_json::json!({ "target": pin_target, "note": "n" })),
        ),
    ];

    for (method, path, body) in &cases {
        let url = format!("http://{addr}{path}");
        let mut req = match *method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            other => panic!("unhandled method {other}"),
        };
        if let Some(b) = body {
            req = req.json(b);
        }
        let status = req.send().await.expect("request").status();
        assert_ne!(
            status,
            reqwest::StatusCode::NOT_FOUND,
            "{method} {path} is not registered in boot::build_router"
        );
        assert_ne!(
            status,
            reqwest::StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} is registered at a different method"
        );
    }

    // `GET /api/investigations` takes no parameters, so it cannot be made to fail before its read.
    // Either outcome here proves the handler was reached: an unregistered route answers 404
    // immediately, and this one either answers something else or blocks on the uninitialised
    // database handle until the client's own timeout.
    let list = client
        .get(format!("http://{addr}/api/investigations"))
        .send()
        .await;
    match list {
        Ok(response) => {
            assert_ne!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND,
                "GET /api/investigations is not registered in boot::build_router"
            );
            assert_ne!(
                response.status(),
                reqwest::StatusCode::METHOD_NOT_ALLOWED,
                "GET /api/investigations is registered at a different method"
            );
        }
        Err(e) => assert!(
            e.is_timeout(),
            "GET /api/investigations failed for a reason other than reaching a handler: {e}"
        ),
    }
}

// ── The bad-date error contract (45-16 Task 1) ───────────────────────────────

/// **A malformed `as_at` is a 400 carrying this project's `{"error": …}` object — the server half
/// of the contract `workbench-web`'s `ProfileLoad::Rejected` reads.**
///
/// Ungated on purpose, and it can be: the date is parsed in `profile_handler` *before*
/// `query_entity_by_id_as_at`, so this request never reaches the uninitialised database handle.
/// That ordering is itself part of the contract — a rejection that had to wait for a database
/// would not be a rejection an analyst could act on.
///
/// Two assertions, because the frontend needs both and either can break alone: the **status** is
/// what `profile_failure_for_status` switches on, and the **body shape** is what
/// `server_error_text` reads. A 400 with a plain-text body would render the generic copy with no
/// detail line and nothing would fail.
///
/// PROVE THIS CAN FAIL: change `profile_handler`'s `as_at` parse to `.unwrap_or(None)` (status
/// becomes 200/404 — run for real), or change `bad_request` to `(StatusCode::BAD_REQUEST,
/// Json(json!(message.to_string())))` so the body is a bare string (the `"error"` key assertion
/// fails — run for real).
#[tokio::test]
async fn a_malformed_as_at_is_rejected_with_the_projects_error_shape() {
    let addr = no_db_router().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    for bad in ["2021-13-45", "30/06/2021", "yesterday", "2021-06"] {
        let response = client
            .get(format!(
                "http://{addr}/api/entity/profile?id={SUBJECT}&as_at={bad}"
            ))
            .send()
            .await
            .expect("the request reaches the handler");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "as_at={bad:?} must be refused, not treated as absent"
        );
        let body: serde_json::Value = response.json().await.expect("a JSON error body");
        let message = body["error"]
            .as_str()
            .unwrap_or_else(|| panic!("the error body must carry an `error` string, got {body}"));
        assert!(
            message.contains("as_at"),
            "the refusal must name the parameter it refused, got {message:?}"
        );
        assert!(
            message.contains(bad),
            "the refusal must quote the value it refused, got {message:?}"
        );
    }

    // Non-vacuity: a well-formed date is NOT refused, so the four cases above are the parse
    // failing rather than this route refusing every `as_at` it is given. (It gets past the parse
    // and then blocks on the uninitialised database handle, so a timeout is the pass here — the
    // one outcome that cannot be a 400.)
    let good = client
        .get(format!(
            "http://{addr}/api/entity/profile?id={SUBJECT}&as_at=2021-06-30"
        ))
        .send()
        .await;
    match good {
        Ok(response) => assert_ne!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "non-vacuity: a well-formed as_at must get past the parse"
        ),
        Err(e) => assert!(e.is_timeout(), "unexpected transport failure: {e}"),
    }
}

// ── The pivot gap, pinned ────────────────────────────────────────────────────

fn unattributed() -> Provenance {
    Provenance::Unattributed {
        reason: "test fixture".to_string(),
    }
}

fn temporal() -> Temporal {
    Temporal {
        range: EffectiveRange::Unrecorded {
            basis: "test fixture".to_string(),
        },
        status: AsAtStatus::NotFiltered,
    }
}

fn bare_profile() -> EntityProfile {
    EntityProfile {
        id: SUBJECT.to_string(),
        name: SUBJECT_NAME.to_string(),
        ch_number: Some(SUBJECT_CH.to_string()),
        status: urban_integrity_schema::types::node::EntityStatus::Active,
        incorporation_date: None,
        identity_provenance: unattributed(),
        addresses: vec![],
        directors: vec![],
        contracts: vec![],
        planning_applications: vec![],
        alerts: vec![],
        related_entities: vec![],
        lpa_presence: vec![],
        beneficial_owners: vec![],
        disqualifications: vec![],
        offshore_links: vec![],
        trustee_of: vec![],
        owns_property: vec![],
        donates_to: vec![],
        osint_links: vec![],
        sanctions_status: None,
        osint_alerts: vec![],
        sub_candidate_links: vec![],
        as_at: None,
    }
}

fn dto_of(profile: &EntityProfile) -> EntityProfileDto {
    let coverage = ProbeCoverage::default();
    let index = findings_naming_entity(
        &profile.id,
        profile.ch_number.as_deref(),
        &[],
        &profile.alerts,
        &coverage,
        None,
    );
    to_dto(
        profile,
        &LabelIndex::default(),
        &index,
        &[],
        &SectionWindow::default(),
    )
    .expect("the mapping must succeed")
}

/// **The pivot surface is exactly as wide as the read path allows, and no wider.**
///
/// 45-01 declared `pivot: Option<EntityRef>` on every row type. 45-08 measured that the core rows
/// carried a destination id on **two**, and wrote this test to fail the day that closed. Plan
/// 45-11's Task 1 (D-06) closed it for three more: `ContractRow`, `PlanningApplicationRow` and
/// `AddressRow` now carry the `AwardedTo`/`LocatedAt` destination id, carried from the graph and
/// never minted, so the measured width is now **five**:
///
/// | Row | Pivot | Kind | Source of the id |
/// |---|---|---|---|
/// | OSINT link | yes | node class | `OsintLink::target_node_id` |
/// | Related entity, company | yes | node class | `GB-COH:{ch_number}` |
/// | Contract | yes | `Contract` | `ContractRow::id`, the `AwardedTo` destination |
/// | Planning application | yes | `Contract` | `PlanningApplicationRow::id`, ditto |
/// | Address | yes | `Location` | `AddressRow::id`, the `LocatedAt` destination |
/// | Officer, property, trustee-of, donates-to, offshore link, beneficial owner, disqualification, related director | **no** | — | the row carries no id |
///
/// Two things this width does **not** assert, both of which 45-10 and 45-11's Task 2 need:
///
/// 1. **A pivot is not a working link.** `query_entity_by_id` opens `Entity`/`OffshoreEntity`
///    nodes only, so every `CF:`/`planning:`/`location:`/`LOC:` id here currently 404s. Pinned in
///    `crates/core/tests/profile_pivot_ids.rs`.
/// 2. `kind` on a pivot comes from the label index (`node_kind_name`), **not** from `RecordKind`,
///    whatever 45-01's comment on `EntityRef` said. D-06 spelled `RecordKind::Contract`/`Location`
///    to match, so the fallback and the indexed answer are one string.
///
/// **When a later plan puts ids on the remaining row types, this test fails.** That is the
/// intended signal: carry the new ids into `profile_map` in the same change and update the
/// assertions that no longer hold (45-03's keep-the-field-to-keep-the-test rule).
///
/// PROVE THIS CAN FAIL: set `pivot: None` in `profile_map::address_row` and the address assertion
/// fails. That mutation was run.
#[test]
fn pivot_coverage_is_what_the_read_path_can_supply() {
    let mut profile = bare_profile();
    profile.contracts = vec![ContractRow {
        id: CONTRACT_ID.to_string(),
        name: "X9999 CF Award".to_string(),
        value_gbp: None,
        status: None,
        published: None,
        ward_code: Some(WARD_CODE.to_string()),
        tender_ref: Some("X9999-TENDER".to_string()),
        application_ref: None,
        provenance: unattributed(),
        temporal: temporal(),
    }];
    profile.planning_applications = vec![PlanningApplicationRow {
        id: PLANNING_ID.to_string(),
        name: "X9999 Change of Use".to_string(),
        status: None,
        published: None,
        ward_code: Some(WARD_CODE.to_string()),
        application_ref: Some("X9999C001".to_string()),
        provenance: unattributed(),
        temporal: temporal(),
    }];
    profile.addresses = vec![AddressRow {
        id: ADDRESS_ID.to_string(),
        postcode: Some("X9 9XX".to_string()),
        address_line_1: Some("1 X9999 Street".to_string()),
        locality: Some("Llangefni".to_string()),
        provenance: unattributed(),
        temporal: temporal(),
    }];
    profile.osint_links = vec![OsintLink {
        target_node_id: "director:x9999-example".to_string(),
        source_system: "ch_psc".to_string(),
        confidence: 1.0,
        resolution_type: "exact_company_number".to_string(),
        status: "confirmed".to_string(),
        provenance: unattributed(),
    }];
    profile.related_entities = vec![
        RelatedEntity {
            name: "X9999 Sister Ltd".to_string(),
            ch_number: Some("X06999002".to_string()),
            status: urban_integrity_schema::types::node::EntityStatus::Active,
            shared_connection: SharedConnection {
                kind: "director".to_string(),
                value: "x9999 example".to_string(),
            },
            provenance: unattributed(),
        },
        RelatedEntity {
            name: "X9999 A Director".to_string(),
            ch_number: None,
            status: urban_integrity_schema::types::node::EntityStatus::Active,
            shared_connection: SharedConnection {
                kind: "address".to_string(),
                value: "X9 9XX".to_string(),
            },
            provenance: unattributed(),
        },
    ];

    let dto = dto_of(&profile);

    // Non-vacuity: every section compared below has a row in it.
    assert_eq!(dto.contracts.rows.len(), 1);
    assert_eq!(dto.planning_applications.rows.len(), 1);
    assert_eq!(dto.addresses.rows.len(), 1);
    assert_eq!(dto.osint_links.rows.len(), 1);
    assert_eq!(dto.related.rows.len(), 2);

    // The three D-06 pivots. Each asserts the id AND the kind: an id with the wrong kind is how a
    // renderer routes a click to the wrong page, and no length check would catch it.
    assert_eq!(
        dto.contracts.rows[0].pivot.as_ref().map(|p| p.id.as_str()),
        Some(CONTRACT_ID),
        "`ContractRow::id` is the AwardedTo destination — carried, not invented"
    );
    assert_eq!(
        dto.contracts.rows[0]
            .pivot
            .as_ref()
            .map(|p| p.kind.as_str()),
        Some("Contract")
    );
    assert_eq!(
        dto.planning_applications.rows[0]
            .pivot
            .as_ref()
            .map(|p| p.id.as_str()),
        Some(PLANNING_ID),
    );
    assert_eq!(
        dto.planning_applications.rows[0]
            .pivot
            .as_ref()
            .map(|p| p.kind.as_str()),
        Some("Contract"),
        "a planning application pivots as a `Contract`: D-03 splits the section, not the node —          both `CF:` and `planning:` destinations are `NodeWeight::Contract`"
    );
    assert_eq!(
        dto.addresses.rows[0].pivot.as_ref().map(|p| p.id.as_str()),
        Some(ADDRESS_ID),
        "`AddressRow::id` is the LocatedAt destination, verbatim — not derived from the postcode"
    );
    assert_eq!(
        dto.addresses.rows[0].pivot.as_ref().map(|p| p.kind.as_str()),
        Some("Location"),
        "`Location` is the graph's name for this node class and `node_kind_name`'s spelling of          it; `Address` is the name of the profile SECTION and must not appear on the wire as a          second spelling of one class"
    );
    assert_eq!(
        dto.osint_links.rows[0]
            .pivot
            .as_ref()
            .map(|p| p.id.as_str()),
        Some("director:x9999-example"),
        "`OsintLink::target_node_id` IS an id, so this pivot is real"
    );
    assert_eq!(
        dto.related.rows[0].pivot.as_ref().map(|p| p.id.as_str()),
        Some("GB-COH:X06999002"),
        "a related company's CH number reconstructs the id queries.rs itself builds"
    );
    assert!(
        dto.related.rows[1].pivot.is_none(),
        "a related row with no CH number has no id to point at"
    );

    // The width, stated as a number so it cannot drift silently: five pivotable row classes.
    let pivotable = [
        dto.contracts.rows[0].pivot.is_some(),
        dto.planning_applications.rows[0].pivot.is_some(),
        dto.addresses.rows[0].pivot.is_some(),
        dto.osint_links.rows[0].pivot.is_some(),
        dto.related.rows[0].pivot.is_some(),
    ];
    assert_eq!(
        pivotable.iter().filter(|p| **p).count(),
        5,
        "45-08 measured two; D-06 made it five. If this number moved, the table in this test's          doc comment moved with it or one of them is now a lie"
    );
}

/// With no probe run in this process, the findings section is `NotSearched` — **not** an empty
/// list that reads as "this entity is clean".
///
/// PROVE THIS CAN FAIL: map `FindingsOutcome::NotSearched` onto `FindingsOutcomeView::NotNamed`
/// in `profile_map::outcome_view` and this test fails. That mutation was run.
#[test]
fn a_profile_with_no_probe_run_says_no_probe_searched() {
    use urban_integrity_workbench_dto::FindingsOutcomeView;

    let dto = dto_of(&bare_profile());
    assert!(dto.findings.findings.rows.is_empty());
    assert_eq!(dto.findings.outcome, FindingsOutcomeView::NotSearched);
}

// ── Round trips: DB-gated ────────────────────────────────────────────────────

/// The profile screen's data, through the real route: every section, the totals, and the paging a
/// **Show more** press performs.
///
/// PROVE THIS CAN FAIL: in `profile_map::section_page`, ignore `window.offset` (always window from
/// 0) and the second-page assertion fails with the first page's rows. That mutation was run.
#[tokio::test]
async fn db_profile_route_returns_sections_totals_and_a_working_cursor() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("entity_api_profile").await;
    let client = reqwest::Client::new();

    let first: EntityProfileDto = client
        .get(format!("http://{addr}/api/entity/profile"))
        .query(&[("id", SUBJECT), ("page_size", "5")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(first.id, SUBJECT);
    assert_eq!(first.name, SUBJECT_NAME);
    assert_eq!(
        first
            .identifiers
            .iter()
            .find(|f| f.value == SUBJECT_CH)
            .map(|f| f.label.as_str()),
        Some("Companies House number")
    );
    assert_eq!(
        first.contracts.total, CONTRACTS as u64,
        "the total is the section, not the window"
    );
    assert_eq!(first.contracts.rows.len(), 5);
    assert!(
        first.planning_applications.rows.is_empty(),
        "a CF-prefixed destination is a contract, not a planning application (D-03)"
    );
    let cursor = first
        .contracts
        .next_cursor
        .clone()
        .expect("11 more contracts exist, so there must be a cursor");

    let second: EntityProfileDto = client
        .get(format!("http://{addr}/api/entity/profile"))
        .query(&[
            ("id", SUBJECT),
            ("page_size", "5"),
            ("section", "contracts"),
            ("cursor", &cursor),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let page_one: BTreeSet<&str> = first
        .contracts
        .rows
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    let page_two: BTreeSet<&str> = second
        .contracts
        .rows
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(page_two.len(), 5);
    assert!(
        page_one.is_disjoint(&page_two),
        "a second page that repeats the first is not a page"
    );
    assert_eq!(second.contracts.total, CONTRACTS as u64);

    // A 404 is the spec's own sentence, so the page renders fixed copy rather than inventing it.
    let missing = client
        .get(format!("http://{addr}/api/entity/profile"))
        .query(&[("id", "GB-COH:X06999999")])
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    let body: serde_json::Value = missing.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("No entity with id"),
        "{body}"
    );
}

/// An as-at date **marks** rows and removes none (PIV-04): the same total, with each row's status
/// moving as the date does.
///
/// **A correction to this test's own first version.** It originally asserted every row came back
/// `Undated`, on the reasoning that "every seeded edge carries a null range". That is true of the
/// edges and false of the rows: `award_range` reads a contract row's range from the *node's*
/// `contract.date_published`, not from the edge's `meta.effective_from`, so a seeded CF award is
/// dated even though the edge that reaches it is not. The gated run said so and the assertion was
/// corrected rather than the code — which makes this the stronger test, because a status that
/// *moves* with the date proves the filter ran, where a uniform `Undated` would also be what a
/// filter that did nothing produced.
#[tokio::test]
async fn db_an_as_at_date_marks_rows_and_removes_none() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("entity_api_as_at").await;
    let client = reqwest::Client::new();

    let profile_as_at = |date: Option<&'static str>| {
        let client = client.clone();
        async move {
            let mut query = vec![("id", SUBJECT.to_string())];
            if let Some(d) = date {
                query.push(("as_at", d.to_string()));
            }
            client
                .get(format!("http://{addr}/api/entity/profile"))
                .query(&query)
                .send()
                .await
                .unwrap()
                .json::<EntityProfileDto>()
                .await
                .unwrap()
        }
    };

    // Unfiltered: no claim is made about any row's relation to a date.
    let unfiltered = profile_as_at(None).await;
    assert_eq!(unfiltered.as_at, None);
    assert_eq!(unfiltered.contracts.total, CONTRACTS as u64);
    assert!(
        unfiltered
            .contracts
            .rows
            .iter()
            .all(|r| r.temporal.status == AsAtStatus::NotFiltered),
        "an unfiltered profile has not established that anything was in force"
    );

    // Before the seeded publication date: positively outside, and STILL PRESENT.
    let before = profile_as_at(Some("2020-01-01")).await;
    assert_eq!(before.as_at.as_deref(), Some("2020-01-01"));
    assert_eq!(
        before.contracts.total, CONTRACTS as u64,
        "an as-at filter must not shorten a section"
    );
    assert_eq!(before.contracts.rows.len(), unfiltered.contracts.rows.len());
    assert!(
        before
            .contracts
            .rows
            .iter()
            .all(|r| r.temporal.status == AsAtStatus::OutOfForce),
        "a row the filter excluded is marked, not removed — which is the only shape in which \
         PIV-04's worked example can be rendered"
    );

    // After it: in force. The status moved, so the filter demonstrably ran.
    let after = profile_as_at(Some("2025-01-01")).await;
    assert!(
        after
            .contracts
            .rows
            .iter()
            .all(|r| r.temporal.status == AsAtStatus::InForce),
        "the same rows, a later date, a different answer"
    );

    let bad = client
        .get(format!("http://{addr}/api/entity/profile"))
        .query(&[("id", SUBJECT), ("as_at", "01/01/2020")])
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);
}

/// PIV-01's list: the status the disambiguation row shows is the status the profile shows, because
/// both come from the same read.
#[tokio::test]
async fn db_disambiguate_route_agrees_with_the_profile_it_opens() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("entity_api_disambiguate").await;
    let client = reqwest::Client::new();

    let list: DisambiguationView = client
        .get(format!("http://{addr}/api/entity/disambiguate"))
        .query(&[("q", "X9999 Example")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        list.rows.len(),
        1,
        "NON-VACUITY: the seeded entity must match"
    );
    assert!(!list.more_may_exist);
    let row = &list.rows[0];
    assert_eq!(row.entity.id, SUBJECT);
    assert_eq!(
        row.ward.as_ref().map(|w| w.id.as_str()),
        Some("ward:X05999001"),
        "the ward comes from the entity's own award rows"
    );

    let profile: EntityProfileDto = client
        .get(format!("http://{addr}/api/entity/profile"))
        .query(&[("id", SUBJECT)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        row.status, profile.status.value,
        "the disambiguation row and the profile it opens must not disagree about status"
    );

    let empty: DisambiguationView = client
        .get(format!("http://{addr}/api/entity/disambiguate"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        empty.rows.is_empty(),
        "no query is an empty list, not an error"
    );
}

/// CNV-01, end to end: the cap bites and the response says so, with the figures the status line is
/// composed from.
///
/// PROVE THIS CAN FAIL: `NeighbourhoodView::cut`'s `if cut == 0` → `if true` (cut the list, report
/// nothing cut) fails this test's `truncated` assertion, and the same mutation fails
/// `a_shortened_list_always_reports_itself_shortened` in `workbench-dto`. That mutation was run.
#[tokio::test]
async fn db_neighbourhood_route_reports_the_cap_when_it_bites() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("entity_api_neighbourhood").await;
    let client = reqwest::Client::new();

    let capped: NeighbourhoodView = client
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[("id", SUBJECT), ("hops", "1"), ("node_cap", "5")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(capped.nodes.len(), 5);
    assert_eq!(
        capped.total_discovered, CONTRACTS as u64,
        "the figure the cut is measured against must survive the cut"
    );
    assert!(capped.truncation.truncated);
    assert_eq!(capped.truncation.reason.as_deref(), Some("node_cap"));
    assert_eq!(capped.node_cap, 5);
    assert!(capped.nodes.iter().all(|n| n.hop == 1));

    let whole: NeighbourhoodView = client
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[("id", SUBJECT), ("hops", "1"), ("node_cap", "250")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(whole.nodes.len(), CONTRACTS);
    assert!(!whole.truncation.truncated);

    // The clamp is the server's: 40 hops is not a hop budget this route will walk.
    let clamped: NeighbourhoodView = client
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[("id", SUBJECT), ("hops", "40"), ("node_cap", "9999999")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        clamped.hops,
        urban_integrity_workbench_dto::caps::CEILING_NEIGHBOURHOOD_HOPS
    );
    assert_eq!(
        clamped.node_cap,
        urban_integrity_workbench_dto::caps::CEILING_NEIGHBOURHOOD_NODE_CAP
    );
}

/// **CNV-02 end to end: an expansion's edges arrive carrying their type AND their recorded range**,
/// through the real router, the real handler and a real database.
///
/// Both live shapes are exercised, because they are rendered differently and only one of them is
/// common: the seeded `AwardedTo` edges carry `effective_from: null, effective_to: null` — which is
/// what **every** `Directs`, `AwardedTo` and `LocatedAt` edge in the working analysis looks like —
/// and one `Controls` edge is inserted here carrying a start date, which is one of the only two
/// edge types in that store that ever does.
///
/// PROVE THIS CAN FAIL: drop the `edge_meta_candidate` call from `NeighbourIndex::from_rows` and
/// pass `None, None`; the `Recorded` assertion fails. Replace `Temporal::unfiltered` in the handler
/// with an `InForce` status and the status assertion fails. Both mutations were run.
#[tokio::test]
async fn db_neighbourhood_edges_carry_their_type_and_the_range_the_record_states() {
    if !db_tests_enabled() {
        return;
    }
    let (ns, addr) = seeded_router("entity_api_neighbourhood_edges").await;

    // One dated edge of a type that really does carry dates on this corpus, inserted after the
    // seed so the seed's own fail-loud count still describes what it wrote.
    let dated = serde_json::json!([{
        "event_id": "x9999-e-controls",
        "timestamp": base_timestamp(),
        "kind": "EdgeAdded",
        "payload": {
            "src_id": SUBJECT,
            "dst_id": "GB-COH:X06999002",
            "weight_json": serde_json::json!({
                "type": "Controls",
                "meta": {
                    "created_at": "2024-01-01T00:00:00Z",
                    "effective_from": "2019-04-01T00:00:00Z",
                    "effective_to": null
                },
                "data": { "details": "seeded" },
            }).to_string(),
        },
    }]);
    ns.query("INSERT INTO graph_events $rows")
        .bind(("rows", dated))
        .await
        .expect("dated edge: query")
        .check()
        .expect("dated edge: statement");

    let view: NeighbourhoodView = reqwest::Client::new()
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[("id", SUBJECT), ("hops", "1"), ("node_cap", "250")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        view.nodes.len(),
        CONTRACTS + 1,
        "the twelve awards and the one controlled company"
    );

    let mut undated = 0;
    let mut recorded = 0;
    for node in &view.nodes {
        assert!(
            !node.edges.is_empty(),
            "an edge is how the walk reached {}",
            node.entity.id
        );
        for edge in &node.edges {
            assert_eq!(
                edge.temporal.status,
                AsAtStatus::NotFiltered,
                "this route runs no as-at filter, so no edge may claim to be in force"
            );
            match (&edge.edge_type[..], &edge.temporal.range) {
                ("AwardedTo", EffectiveRange::Unrecorded { basis }) => {
                    assert_eq!(
                        basis, "AwardedTo.meta.effective_from / AwardedTo.meta.effective_to",
                        "an unrecorded range must name the fields it read and found empty"
                    );
                    undated += 1;
                }
                ("Controls", EffectiveRange::Recorded { from, to, basis }) => {
                    assert_eq!(from.as_deref(), Some("2019-04-01T00:00:00Z"));
                    assert_eq!(
                        *to, None,
                        "no end was recorded, and `None` here means exactly that"
                    );
                    assert_eq!(
                        basis,
                        "Controls.meta.effective_from / Controls.meta.effective_to"
                    );
                    recorded += 1;
                }
                (ty, range) => panic!("unexpected edge {ty} with range {range:?}"),
            }
        }
    }
    // Non-vacuity on BOTH arms: a run that saw only undated edges would prove nothing about the
    // dated path, and one that saw only the dated edge nothing about the common case.
    assert_eq!(undated, CONTRACTS, "the undated population must be real");
    assert_eq!(recorded, 1, "the dated population must be real");
}

// ── Multi-seed union + hub exclusion (Phase 50, plan 50-04, D-12/T-50-02) ────────────────────

const SUBJECT2: &str = "GB-COH:X06999200";
const SUBJECT2_NEIGHBOUR: &str = "GB-COH:X06999201";

/// A second, disconnected entity with its own single neighbour, seeded alongside whatever
/// `seeded_router` already wrote — SUBJECT and SUBJECT2 share no direct edge, so a `hops=1`
/// expansion from each proves nothing about the other unless the response is a real union.
async fn seed_second_subject(db: &Surreal<Any>) {
    let rows = vec![
        node_row(
            "x9999-subject2",
            SUBJECT2,
            serde_json::json!({
                "type": "Entity",
                "name": "X9999 Second Subject Ltd",
                "company_number": "X06999200",
                "status": { "type": "Active" },
            }),
        ),
        node_row(
            "x9999-subject2-neighbour",
            SUBJECT2_NEIGHBOUR,
            serde_json::json!({
                "type": "Entity",
                "name": "X9999 Second Subject's Neighbour Ltd",
                "company_number": "X06999201",
                "status": { "type": "Active" },
            }),
        ),
        edge_row("x9999-e-subject2", SUBJECT2, SUBJECT2_NEIGHBOUR),
    ];
    db.query("INSERT INTO graph_events $rows")
        .bind(("rows", serde_json::Value::Array(rows)))
        .await
        .expect("second subject: query")
        .check()
        .expect("second subject: statement");
}

/// D-12 end to end: `ids=` unions two seeds' walks into one response, deduplicated by node id,
/// and both named ids appear in the response's own `seeds`.
///
/// PROVE THIS CAN FAIL: drop `params.ids` from `collect_seed_ids`'s call in
/// `neighbourhood_handler` (fall back to `id` alone) and `view.nodes.len()` comes back
/// `CONTRACTS` short one, with `SUBJECT2_NEIGHBOUR` absent. That mutation was run.
#[tokio::test]
async fn db_neighbourhood_route_accepts_multiple_seed_ids() {
    if !db_tests_enabled() {
        return;
    }
    let (ns, addr) = seeded_router("entity_api_neighbourhood_multiseed").await;
    seed_second_subject(&ns).await;

    let ids_param = format!("{SUBJECT},{SUBJECT2}");
    let view: NeighbourhoodView = reqwest::Client::new()
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[
            ("ids", ids_param.as_str()),
            ("hops", "1"),
            ("node_cap", "250"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        view.seeds.len(),
        2,
        "both named ids must appear in the response's own seed list: {:?}",
        view.seeds
    );
    let seed_ids: Vec<&str> = view.seeds.iter().map(|s| s.id.as_str()).collect();
    assert!(seed_ids.contains(&SUBJECT));
    assert!(seed_ids.contains(&SUBJECT2));

    assert_eq!(
        view.nodes.len(),
        CONTRACTS + 1,
        "the union of SUBJECT's twelve contracts and SUBJECT2's one neighbour"
    );
    assert_eq!(view.total_discovered, (CONTRACTS + 1) as u64);
    assert!(!view.truncation.truncated);

    let node_ids: Vec<&str> = view.nodes.iter().map(|n| n.entity.id.as_str()).collect();
    for i in 0..CONTRACTS {
        let cid = contract_id(i);
        assert!(node_ids.contains(&cid.as_str()), "missing {cid}");
    }
    assert!(node_ids.contains(&SUBJECT2_NEIGHBOUR));
}

const HUB_SUBJECT: &str = "GB-COH:X06999300";
const HUB_NODE: &str = "GB-COH:X06999301";
const HUB_LEAF_COUNT: usize = 20;
/// Isolated CSR nodes with no edges at all, existing ONLY to size the population large enough
/// that nearest-rank p99 does not degenerate to the population's own max degree — see
/// [`hub_seeded_router`]'s doc comment for the arithmetic.
const HUB_FILLER_COUNT: usize = 38;

fn hub_leaf_id(i: usize) -> String {
    format!("GB-COH:X0699941{i:02}")
}

fn hub_filler_id(i: usize) -> String {
    format!("GB-COH:X0699951{i:02}")
}

/// A CSR-and-`graph_events`-matched fixture for T-50-02, deliberately NOT built from
/// `seeded_router` — that fixture's CSR half is empty (`workbench_from_graph_with_db(&[], &[],
/// ...)`), which is fine for every other route in this file (they read `graph_events` alone) but
/// useless here: the degree cap [`resolve_hub_ids`] computes is measured over the CSR snapshot,
/// not over `graph_events`, so a hub test needs both sides populated with the SAME shape.
///
/// `HUB_NODE` carries 21 edges (1 to `HUB_SUBJECT`, 20 to its leaves) — the unique maximum degree
/// in a 60-node population where every other node has degree 0 (the 38 filler nodes) or 1
/// (`HUB_SUBJECT` and the 20 leaves). Nearest-rank p99 over 60 sorted degrees lands at index
/// `round(0.99 * 59) = 58`, which falls inside the 21-wide block of degree-1 nodes (indices
/// 38..=58) and never on the lone degree-21 entry at index 59 — so the cutoff is 1 and
/// `resolve_hub_ids`'s `degree > cutoff` filter admits `HUB_NODE` and nothing else, regardless of
/// exactly where in that block the rank falls. Below 51 total nodes the rank would land ON the
/// max itself and no node would ever clear its own cutoff — the phase 50 assumptions notes' planted-fixture
/// note names the same degeneracy on an 86-node population.
///
/// The filler nodes are CSR-only (never written to `graph_events`): nothing in this test's own
/// walk, starting from `HUB_SUBJECT`, can ever reach them, so they exist purely to size the
/// population.
async fn hub_seeded_router(db_name: &str) -> (TestNs, std::net::SocketAddr) {
    let ns = TestNs::connect(db_name).await;

    // `graph_events` side: only the reachable subgraph — `load_neighbour_index` reads `EdgeAdded`
    // rows alone, so no `NodeAdded` row is needed for this route's own read path.
    let mut rows = vec![edge_row("x9999-hub-e-subject-hub", HUB_SUBJECT, HUB_NODE)];
    for i in 0..HUB_LEAF_COUNT {
        rows.push(edge_row(
            &format!("x9999-hub-e-leaf-{i:02}"),
            HUB_NODE,
            &hub_leaf_id(i),
        ));
    }
    ns.query("INSERT INTO graph_events $rows")
        .bind(("rows", serde_json::Value::Array(rows)))
        .await
        .expect("hub fixture: query")
        .check()
        .expect("hub fixture: statement");

    // CSR side: the same reachable shape, plus the isolated filler nodes this fixture's doc
    // comment explains.
    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }
    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }
    fn awarded_to() -> EdgeWeight {
        EdgeWeight::AwardedTo {
            meta: meta(),
            data: AwardedToData {
                details: "seeded".to_string(),
            },
        }
    }

    let mut nodes = vec![
        (
            EntityId(HUB_SUBJECT.to_string()),
            entity("X9999 Hub Subject Ltd"),
        ),
        (EntityId(HUB_NODE.to_string()), entity("X9999 Hub Node Ltd")),
    ];
    let mut edges = vec![(
        EntityId(HUB_SUBJECT.to_string()),
        EntityId(HUB_NODE.to_string()),
        awarded_to(),
    )];
    for i in 0..HUB_LEAF_COUNT {
        let leaf = hub_leaf_id(i);
        nodes.push((
            EntityId(leaf.clone()),
            entity(&format!("X9999 Hub Leaf {i:02} Ltd")),
        ));
        edges.push((EntityId(HUB_NODE.to_string()), EntityId(leaf), awarded_to()));
    }
    for i in 0..HUB_FILLER_COUNT {
        let filler = hub_filler_id(i);
        nodes.push((
            EntityId(filler),
            entity(&format!("X9999 Hub Filler {i:02} Ltd")),
        ));
    }

    let (handle, state) = workbench_from_graph_with_db(&nodes, &edges, &[], ns.arc()).await;
    let addr = spawn_router(&handle, &state).await;
    (ns, addr)
}

/// T-50-02 end to end: a hub node is included at the hop it was reached, but the walk never
/// crosses ITS edges to go further — the mitigation lives inside the traversal itself, not only
/// in a node-cap report applied after the fact.
///
/// PROVE THIS CAN FAIL: pass an empty set through where `neighbourhood_handler` reads
/// `resolve_hub_ids` (i.e. never apply the hub filter) and all 20 leaves reappear at hop 2, with
/// `view.nodes.len()` coming back 21 instead of 1. That mutation was run.
#[tokio::test]
async fn db_neighbourhood_route_excludes_further_expansion_through_a_hub_node() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = hub_seeded_router("entity_api_neighbourhood_hub").await;

    let view: NeighbourhoodView = reqwest::Client::new()
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[("id", HUB_SUBJECT), ("hops", "2"), ("node_cap", "250")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(
        view.nodes.len(),
        1,
        "only the hub node itself is reachable — its 20 leaves must never be walked to: {:?}",
        view.nodes.iter().map(|n| &n.entity.id).collect::<Vec<_>>()
    );
    assert_eq!(view.nodes[0].entity.id, HUB_NODE);
    assert_eq!(
        view.nodes[0].hop, 1,
        "the hub is reached at hop 1, straight from the subject"
    );

    let leaf_ids: BTreeSet<String> = (0..HUB_LEAF_COUNT).map(hub_leaf_id).collect();
    assert!(
        view.nodes.iter().all(|n| !leaf_ids.contains(&n.entity.id)),
        "no leaf, reachable only through the hub, may appear in the response"
    );
    assert!(
        !view.truncation.truncated,
        "nothing here was cut by the node cap — the hub's further edges were simply never read"
    );
}

/// PIV-05's whole lifecycle through the routes wave 4 and 5 will call: create, pin, note, reopen,
/// list, unpin, delete — plus the two no-op cases that must not report success.
#[tokio::test]
async fn db_investigation_routes_round_trip_a_whole_lifecycle() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("entity_api_investigation").await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let created: SavedInvestigationView = client
        .post(format!("{base}/api/investigations"))
        .json(&serde_json::json!({ "name": "X9999 follow-up", "notes": "" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.name, "X9999 follow-up");
    let id = created.id.clone();

    let target = serde_json::json!({ "kind": "entity", "entity_id": SUBJECT });
    let pinned: SavedPinView = client
        .post(format!("{base}/api/investigation/pin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "target": target, "origin": "profile", "note": null }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pinned.origin, PinOriginView::Profile);

    // Idempotent: pinning the same entity twice does not create two entries (PIV-05).
    let repinned: SavedPinView = client
        .post(format!("{base}/api/investigation/pin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "target": target, "origin": "canvas", "note": null }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repinned.id, pinned.id);
    assert_eq!(
        repinned.pinned_at, pinned.pinned_at,
        "a re-pin keeps saying when the member was first found"
    );

    let noted: SavedPinView = client
        .patch(format!("{base}/api/investigation/pin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "target": target, "note": "twelve awards in one ward" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(noted.note, "twelve awards in one ward");

    let edge_target = serde_json::json!({
        "kind": "edge", "from": SUBJECT, "to": contract_id(0), "relation": "awarded_to",
    });
    let edge_pin = client
        .post(format!("{base}/api/investigation/pin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "target": edge_target, "origin": "subgraph", "note": null }))
        .send()
        .await
        .unwrap();
    assert!(edge_pin.status().is_success());

    // Reopening restores the pinned set and every note (PIV-05) — a fresh request, not state held
    // in this process.
    let detail: InvestigationDetailView = client
        .get(format!("{base}/api/investigation"))
        .query(&[("id", &id)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail.entity_count, 1);
    assert_eq!(detail.edge_count, 1);
    assert_eq!(detail.pins.len(), 2);
    assert!(detail
        .pins
        .iter()
        .any(|p| p.note == "twelve awards in one ward"));
    assert!(detail.pins.iter().any(
        |p| matches!(&p.target, PinTargetView::Edge { relation, .. } if relation == "awarded_to")
    ));

    let list: Vec<InvestigationSummaryView> = client
        .get(format!("{base}/api/investigations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list
        .iter()
        .find(|r| r.investigation.id == id)
        .expect("the created investigation must be in the list");
    assert_eq!(row.entity_count, 1);
    assert_eq!(row.edge_count, 1);

    let renamed: SavedInvestigationView = client
        .patch(format!("{base}/api/investigation"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "name": "X9999 follow-up (renamed)", "notes": null }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(renamed.name, "X9999 follow-up (renamed)");

    // An update that changes nothing must not report that something was saved.
    let nothing = client
        .patch(format!("{base}/api/investigation"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "name": null, "notes": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(nothing.status(), reqwest::StatusCode::BAD_REQUEST);

    // Unpinning something that was never pinned is an honest `removed: false`, not a success.
    let never: UnpinResultView = client
        .post(format!("{base}/api/investigation/unpin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({
            "target": { "kind": "entity", "entity_id": "GB-COH:X06999999" }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!never.removed);

    let removed: UnpinResultView = client
        .post(format!("{base}/api/investigation/unpin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "target": target }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(removed.removed);

    let deleted = client
        .delete(format!("{base}/api/investigation"))
        .query(&[("id", &id)])
        .send()
        .await
        .unwrap();
    assert!(deleted.status().is_success());

    // Everything that names a gone investigation is a 404, not a 200 over nothing.
    let gone = client
        .get(format!("{base}/api/investigation"))
        .query(&[("id", &id)])
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), reqwest::StatusCode::NOT_FOUND);
    let note_on_gone = client
        .patch(format!("{base}/api/investigation/pin"))
        .query(&[("id", &id)])
        .json(&serde_json::json!({ "target": target, "note": "lost" }))
        .send()
        .await
        .unwrap();
    assert_eq!(note_on_gone.status(), reqwest::StatusCode::NOT_FOUND);
}
