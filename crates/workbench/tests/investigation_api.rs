//! Plan 45-15 (PIV-05) through the router: **a saved investigation survives a workbench restart,
//! and pinning the same entity twice does not create two entries.**
//!
//! # Why this is the requirement's real test
//!
//! The phase 45 UI spec: *"Reopening restores the pinned set and every note. This persists in the
//! **analysis database**, not browser storage — phases 47 and 48 consume the same primitive
//! server-side, and `localStorage` would be invisible to them."*
//!
//! So the claim is not "the page re-renders". It is that nothing the investigation needs lives in
//! a process. [`db_an_investigation_survives_a_workbench_restart`] therefore builds **two
//! separately-assembled workbench servers** over one store: it writes through the first, drops
//! every handle the first held — the router task, the `WorkbenchState`, the graph coordinator and
//! the SurrealDB connection — and reads through a second built from scratch on a different port
//! with its own connection. A browser-local store, an in-process cache or a static would all fail
//! this; an `is_ok()` round trip would pass through every one of them.
//!
//! It does **not** restart SurrealDB itself, for the reason `crates/core/tests/saved_investigations.rs`
//! gives: the instance on this box serves the live analysis store, and a `scripts/surreal-start.sh`
//! run from the wrong directory silently creates an empty one (docs/DEVELOPMENT.md). What the requirement
//! needs is that nothing is in the *workbench*, and two workbenches over one store is that claim.
//!
//! # Why the idempotency test is here as well as in `crates/core`
//!
//! 45-07 proved `pin_member` keeps one row for one member. That is the storage layer. This proves
//! the property survives the surface an analyst actually uses: **two pins of the same entity taken
//! from two different screens, under two different date windows, through the HTTP route the
//! browser calls.** The failure it guards against is not in the write layer at all — it is a UI
//! that puts something else in the identity (the as-at window being the live candidate, recommended
//! by 45-12 §8 and declined in 45-15 §5), which the storage layer would faithfully store as a
//! second member.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-workbench --test investigation_api
//! ```
//!
//! Both tests are `db_`-prefixed and skip by returning, so a gated run reports the same passing
//! count as an ungated one. The namespace comes from `urban-integrity-test-support` and is removed
//! when the guard drops.

use std::sync::Arc;

use surrealdb::engine::any::Any;
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;
use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, RepresentsData};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, NodeWeight, WardData};
use urban_integrity_test_support::{credentials, db_tests_enabled, endpoint, NsGuard};
use urban_integrity_workbench::boot::WorkbenchState;
use urban_integrity_workbench::graph::fixture::workbench_from_graph_with_db;
use urban_integrity_workbench_dto::investigation::{
    CreateInvestigationRequest, InvestigationDetailView, PinNoteRequest, PinOriginView, PinRequest,
    PinTargetView, SavedInvestigationView, SavedPinView, UpdateInvestigationRequest,
};

// ── Synthetic ids only: `scripts/check-no-analysis-literals.sh` forbids an analysis-specific
// literal anywhere in this crate, and an `X05…` ward code can never name a real ONS ward. ──

const COMPANY: &str = "GB-COH:X06999001";
const DIRECTOR: &str = "director:x9999-example-officer";
const WARD: &str = "ward:X05999001";
const DB_NAME: &str = "an_x9999_investigation";

/// A second, independent client on the same namespace and database — what "a restarted workbench"
/// means here. Shares nothing with the connection that wrote.
async fn fresh_connection(ns: &str) -> Arc<Surreal<Any>> {
    let (username, password) = credentials();
    let db = surrealdb::engine::any::connect(endpoint())
        .await
        .expect("SurrealDB unreachable");
    db.signin(Root { username, password })
        .await
        .expect("SurrealDB root sign-in failed");
    db.use_ns(ns)
        .use_db(DB_NAME)
        .await
        .expect("namespace/database selection failed");
    db.query(urban_integrity_schema::db::SCHEMA_DEF)
        .await
        .expect("schema init failed")
        .check()
        .expect("schema init returned a statement error");
    Arc::new(db)
}

fn meta() -> EdgeMeta {
    EdgeMeta {
        created_at: chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        effective_from: None,
        effective_to: None,
    }
}

/// A graph small enough to be beside the point and real enough that the router assembles.
#[allow(clippy::type_complexity)]
fn fixture_graph() -> (
    Vec<(EntityId, NodeWeight)>,
    Vec<(EntityId, EntityId, EdgeWeight)>,
) {
    let nodes = vec![
        (
            EntityId(COMPANY.to_string()),
            NodeWeight::Entity(EntityData {
                name: "X9999 Example Holdings Ltd".to_string(),
                company_number: Some("X06999001".to_string()),
                status: Default::default(),
                incorporation_date: None,
            }),
        ),
        (
            EntityId(DIRECTOR.to_string()),
            NodeWeight::Entity(EntityData {
                name: "X9999 Example Officer".to_string(),
                company_number: None,
                status: Default::default(),
                incorporation_date: None,
            }),
        ),
        (
            EntityId(WARD.to_string()),
            NodeWeight::Ward(WardData {
                name: "X9999 Example Ward".to_string(),
            }),
        ),
    ];
    let edges = vec![(
        EntityId(COMPANY.to_string()),
        EntityId(WARD.to_string()),
        EdgeWeight::Represents {
            meta: meta(),
            data: RepresentsData {
                details: "seeded".to_string(),
            },
        },
    )];
    (nodes, edges)
}

/// Assemble one whole workbench — coordinator, snapshot, state, router — over `db`, and serve it.
///
/// Returns the address and the handles, so a caller can drop **everything** the server held and
/// prove the next one shares none of it.
async fn spawn_workbench(
    db: Arc<Surreal<Any>>,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<GraphCoordinatorHandle>,
    Arc<WorkbenchState>,
) {
    let (nodes, edges) = fixture_graph();
    let (handle, state) = workbench_from_graph_with_db(&nodes, &edges, &[], db).await;
    let router =
        urban_integrity_workbench::boot::build_router(Arc::clone(&handle), Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (addr, task, handle, state)
}

fn entity_target(id: &str) -> PinTargetView {
    PinTargetView::Entity {
        entity_id: id.to_string(),
    }
}

async fn create(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    name: &str,
    notes: &str,
) -> SavedInvestigationView {
    let response = client
        .post(format!("http://{addr}/api/investigations"))
        .json(&CreateInvestigationRequest {
            name: name.to_string(),
            notes: notes.to_string(),
        })
        .send()
        .await
        .expect("create: request");
    assert!(
        response.status().is_success(),
        "create: {:?}",
        response.status()
    );
    response.json().await.expect("create: body")
}

async fn pin(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    id: &str,
    entity_id: &str,
    origin: PinOriginView,
    note: Option<&str>,
) -> SavedPinView {
    let response = client
        .post(format!("http://{addr}/api/investigation/pin?id={id}"))
        .json(&PinRequest {
            target: entity_target(entity_id),
            origin,
            note: note.map(str::to_string),
        })
        .send()
        .await
        .expect("pin: request");
    assert!(
        response.status().is_success(),
        "pin: {:?}",
        response.status()
    );
    response.json().await.expect("pin: body")
}

async fn reopen(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    id: &str,
) -> InvestigationDetailView {
    let response = client
        .get(format!("http://{addr}/api/investigation?id={id}"))
        .send()
        .await
        .expect("reopen: request");
    assert!(
        response.status().is_success(),
        "reopen: {:?}",
        response.status()
    );
    response.json().await.expect("reopen: body")
}

fn note_of<'a>(detail: &'a InvestigationDetailView, entity_id: &str) -> &'a str {
    detail
        .pins
        .iter()
        .find(|p| p.target == entity_target(entity_id))
        .map(|p| p.note.as_str())
        .unwrap_or_else(|| panic!("{entity_id} must be a member"))
}

/// **The requirement: the workbench restarts and the investigation is still there, whole.**
///
/// Written so it cannot pass vacuously. "The investigation exists" and "the set is non-empty" would
/// both be true of a server that never dropped anything, so neither is the assertion. What is
/// asserted is that a **second, independently assembled workbench** — different connection,
/// different coordinator, different snapshot, different port — returns the name, the notes, both
/// members, both members' notes, both origins and the original `pinned_at`.
///
/// Two non-vacuity checks come first:
///
/// 1. every handle the writing server held is dropped, and the drop is what makes the second read
///    a claim about the store rather than about a cache;
/// 2. a **third** workbench, assembled the same way over a different namespace, answers 404 for the
///    same id — so the answer demonstrably comes from the rows in this namespace and not from
///    anything ambient in the process.
///
/// PROVE THIS CAN FAIL: in `core::persistence::saved_investigations::pin_member`, bind
/// `("note", None::<String>)` instead of the caller's note — the note assertions fail while every
/// count still matches, which is exactly the shape of "the set came back but the work did not".
#[tokio::test]
async fn db_an_investigation_survives_a_workbench_restart() {
    if !db_tests_enabled() {
        return;
    }
    // `NsGuard` rather than `TestNs`: the namespace must outlive the first connection, and `TestNs`
    // removes it when its handle drops. The guard is bound for the whole test and cleans up at the
    // end.
    let guard = NsGuard::new();
    let client = reqwest::Client::new();

    // ── The first workbench ─────────────────────────────────────────────────────────────────
    let first_db = fresh_connection(guard.name()).await;
    let (first_addr, first_task, first_handle, first_state) =
        spawn_workbench(Arc::clone(&first_db)).await;

    let investigation = create(
        &client,
        first_addr,
        "Ward award concentration",
        "why this set exists",
    )
    .await;
    let id = investigation.id.clone();

    // Pinned from a profile, with a note.
    pin(
        &client,
        first_addr,
        &id,
        COMPANY,
        PinOriginView::Profile,
        Some("the applicant on three of the awards"),
    )
    .await;
    // Pinned from the canvas, with no note — then given one through the note route, which is what
    // the editor does on blur.
    pin(
        &client,
        first_addr,
        &id,
        DIRECTOR,
        PinOriginView::Canvas,
        None,
    )
    .await;
    let noted = client
        .patch(format!("http://{first_addr}/api/investigation/pin?id={id}"))
        .json(&PinNoteRequest {
            target: entity_target(DIRECTOR),
            note: "also directs the second bidder".to_string(),
        })
        .send()
        .await
        .expect("pin note: request");
    assert!(
        noted.status().is_success(),
        "pin note: {:?}",
        noted.status()
    );

    // And the investigation's own notes, edited after creation.
    let renoted = client
        .patch(format!("http://{first_addr}/api/investigation?id={id}"))
        .json(&UpdateInvestigationRequest {
            name: None,
            notes: Some("why this set exists, revised".to_string()),
        })
        .send()
        .await
        .expect("notes: request");
    assert!(
        renoted.status().is_success(),
        "notes: {:?}",
        renoted.status()
    );

    // Read it once through the server that wrote it, so the "before" is a measured fact rather
    // than an assumption about what the writes did.
    let before = reopen(&client, first_addr, &id).await;
    assert_eq!(
        before.entity_count, 2,
        "non-vacuity: two members were written"
    );
    assert_eq!(before.edge_count, 0);
    assert_eq!(before.investigation.notes, "why this set exists, revised");
    assert_eq!(
        note_of(&before, COMPANY),
        "the applicant on three of the awards"
    );
    assert_eq!(note_of(&before, DIRECTOR), "also directs the second bidder");
    let pinned_at_before: Vec<String> = before.pins.iter().map(|p| p.pinned_at.clone()).collect();

    // ── The restart: everything the first workbench held goes away ──────────────────────────
    first_task.abort();
    drop(first_state);
    drop(first_handle);
    drop(first_db);

    // ── The second workbench: its own connection, its own coordinator, its own port ─────────
    let second_db = fresh_connection(guard.name()).await;
    let (second_addr, second_task, _second_handle, _second_state) =
        spawn_workbench(Arc::clone(&second_db)).await;
    assert_ne!(
        first_addr, second_addr,
        "non-vacuity: the second workbench must be a different server"
    );

    let after = reopen(&client, second_addr, &id).await;

    assert_eq!(after.investigation.name, "Ward award concentration");
    assert_eq!(
        after.investigation.notes, "why this set exists, revised",
        "the analyst's investigation notes must survive the restart"
    );
    assert_eq!(after.investigation.analysis_db, DB_NAME);
    assert_eq!(after.investigation.created_at, investigation.created_at);
    assert_eq!(after.entity_count, 2);
    assert_eq!(after.edge_count, 0);

    let members: Vec<&PinTargetView> = after.pins.iter().map(|p| &p.target).collect();
    assert!(members.contains(&&entity_target(COMPANY)));
    assert!(members.contains(&&entity_target(DIRECTOR)));

    assert_eq!(
        note_of(&after, COMPANY),
        "the applicant on three of the awards",
        "a member's note is analyst work product and must survive the restart"
    );
    assert_eq!(note_of(&after, DIRECTOR), "also directs the second bidder");

    let origins: std::collections::BTreeMap<&str, PinOriginView> = after
        .pins
        .iter()
        .map(|p| match &p.target {
            PinTargetView::Entity { entity_id } => (entity_id.as_str(), p.origin),
            PinTargetView::Edge { .. } => unreachable!("no edge pins in this fixture"),
        })
        .collect();
    assert_eq!(origins[COMPANY], PinOriginView::Profile);
    assert_eq!(
        origins[DIRECTOR],
        PinOriginView::Canvas,
        "which surface a member came from is part of the set, not a display detail"
    );

    let pinned_at_after: Vec<String> = after.pins.iter().map(|p| p.pinned_at.clone()).collect();
    assert_eq!(
        pinned_at_after, pinned_at_before,
        "`pinned_at` says when a member was FIRST found; a restart must not restate it"
    );

    // ── Non-vacuity: the answer came from THIS namespace's rows ─────────────────────────────
    let elsewhere = NsGuard::new();
    let elsewhere_db = fresh_connection(elsewhere.name()).await;
    let (elsewhere_addr, elsewhere_task, _h, _s) = spawn_workbench(Arc::clone(&elsewhere_db)).await;
    let missing = client
        .get(format!("http://{elsewhere_addr}/api/investigation?id={id}"))
        .send()
        .await
        .expect("elsewhere: request");
    assert_eq!(
        missing.status().as_u16(),
        404,
        "an identically-assembled workbench over a different store must not know this \
         investigation — otherwise the read above proves nothing about where it was stored"
    );

    elsewhere_task.abort();
    second_task.abort();
}

/// **Pinning the same entity twice does not create two entries — through the route the browser
/// calls, from two different surfaces.**
///
/// This plan's third truth. 45-07 proved it of the write layer; this proves the surface cannot
/// defeat it, which is where the live candidate for defeating it was: 45-12 §8 recommended storing
/// the as-at window with a pin, and putting it in the *identity* would make one company under two
/// windows two members of one set. The browser sends the same target both times
/// (`workbench-web::investigation::entity_pin_target`, which takes the id and nothing else), and
/// this is the server-side half of that claim.
///
/// PROVE THIS CAN FAIL: make the second pin's target
/// `PinTargetView::Entity { entity_id: format!("{COMPANY}@2021-06-30") }` — the member-count
/// assertion fails with two entries for one subject, which is what an as-at in the identity would
/// produce.
#[tokio::test]
async fn db_pinning_one_entity_from_two_surfaces_keeps_one_member() {
    if !db_tests_enabled() {
        return;
    }
    let guard = NsGuard::new();
    let db = fresh_connection(guard.name()).await;
    let (addr, task, _handle, _state) = spawn_workbench(Arc::clone(&db)).await;
    let client = reqwest::Client::new();

    let investigation = create(&client, addr, "one subject, two views", "").await;
    let id = investigation.id.clone();

    // First pin: from the profile, while an as-at window of one date was on screen.
    let first = pin(
        &client,
        addr,
        &id,
        COMPANY,
        PinOriginView::Profile,
        Some("found under a 2021 window"),
    )
    .await;
    let one = reopen(&client, addr, &id).await;
    assert_eq!(one.entity_count, 1, "non-vacuity: one member after one pin");

    // Second pin: the same company, from the canvas, where no as-at filter runs at all, and with
    // no note — the shape the pin control actually sends (`note: None` preserves).
    let second = pin(&client, addr, &id, COMPANY, PinOriginView::Canvas, None).await;

    let two = reopen(&client, addr, &id).await;
    assert_eq!(
        two.entity_count, 1,
        "pinning the same entity twice must not create two entries (PIV-05)"
    );
    assert_eq!(two.pins.len(), 1);
    assert_eq!(second.id, first.id, "the same member is the same record");
    assert_eq!(
        note_of(&two, COMPANY),
        "found under a 2021 window",
        "a re-pin from another surface must not erase the note the analyst typed"
    );
    assert_eq!(
        second.pinned_at, first.pinned_at,
        "`pinned_at` records when the member was first found"
    );
    assert_eq!(
        two.pins[0].origin,
        PinOriginView::Canvas,
        "the newer surface is recorded — the set says where it was last seen, not twice"
    );

    // Non-vacuity for the equality above: a DIFFERENT entity is a different member, so the
    // one-member assertions are about deduplication rather than about a route that pins nothing.
    pin(&client, addr, &id, DIRECTOR, PinOriginView::Profile, None).await;
    let three = reopen(&client, addr, &id).await;
    assert_eq!(
        three.entity_count, 2,
        "a different subject must still be a second member"
    );

    task.abort();
}
