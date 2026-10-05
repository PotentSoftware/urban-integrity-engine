//! The entity-profile screen's golden tests against the planted fixture (plan 45-16 Task 2).
//!
//! # Why this binary exists, and why it is not in `planted_golden.rs`
//!
//! `planted_golden.rs` proves the six graph algorithms against the same manifest with **no
//! database**: `dispatch::run_algorithm` reads the in-memory hot graph, so a fixture built from
//! `planted_graph()` is the whole world it needs. The profile read path is the other half of the
//! workbench and it does not work that way — `query_entity_by_id` scans `graph_events`, so a
//! profile has no answer at all until the fixture is written to a store. These tests are
//! therefore `URBAN_DB_TESTS=1`-gated, which is also why they live in their own binary: a gate
//! that turned `planted_golden.rs` amber would hide six ungated algorithm goldens behind a
//! database.
//!
//! # What these prove that the manifest alone cannot
//!
//! The manifest's profile numbers are **literals written by hand from `topology()`**, exactly like
//! its algorithm answers. That is deliberate and it is also the weakness: a profile count is a
//! property of the topology *and* of the read path's direction conventions, and hand-deriving one
//! without checking it is how a plausible-looking wrong number reaches a QA checklist that the
//! operator then trusts over the screen. So every expectation here is read back through
//! **`boot::build_router` over a real socket** — the same route, method and JSON the browser will
//! use — rather than by calling the query function beneath it.
//!
//! The operator's entity-profile QA checklist quotes these numbers. If one of them is wrong, the
//! toy block sends the operator looking for a defect that is not there, which is worse than no toy
//! block at all.
//!
//! Namespaces come from `urban-integrity-test-support` (docs/TESTING.md's mandatory rule), and the guard
//! stays bound for the whole test because dropping it removes the namespace.

use std::sync::Arc;
use std::time::Duration;

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_test_support::events::seed_planted;
use urban_integrity_test_support::planted::{
    planted_graph, ExpectedProfile, PlantedGraph, WARD_CODE,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};
use urban_integrity_workbench::boot::WorkbenchState;
use urban_integrity_workbench::graph::fixture::workbench_from_graph_with_db;
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;
use urban_integrity_workbench_dto::temporal::AsAtStatus;
use urban_integrity_workbench_dto::{DisambiguationView, EntityProfileDto};

// ── Fixture ──────────────────────────────────────────────────────────────────

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

/// A router over a namespace holding the planted fixture, with the same hot graph the canvas
/// would have. Both halves are needed: the profile route reads `graph_events`, and
/// `entity_ref_for` reads the label index built from the node list.
async fn planted_router(db_name: &str) -> (TestNs, std::net::SocketAddr, PlantedGraph) {
    let ns = TestNs::connect(db_name).await;
    let graph = planted_graph();
    seed_planted(&ns, &graph)
        .await
        .expect("the planted fixture must seed");
    let edges: Vec<_> = graph
        .edges
        .iter()
        .map(|e| (e.src.clone(), e.dst.clone(), e.weight.clone()))
        .collect();
    let (handle, state) =
        workbench_from_graph_with_db(&graph.nodes, &edges, &[WARD_CODE.to_string()], ns.arc())
            .await;
    let addr = spawn_router(&handle, &state).await;
    (ns, addr, graph)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

async fn get_profile(
    addr: std::net::SocketAddr,
    id: &str,
    as_at: Option<&str>,
) -> EntityProfileDto {
    let mut url = format!(
        "http://{addr}/api/entity/profile?id={}",
        urlencoding_encode(id)
    );
    if let Some(date) = as_at {
        url.push_str(&format!("&as_at={date}"));
    }
    let response = client().get(&url).send().await.expect("profile request");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "GET profile {id} (as_at={as_at:?}) must be 200, body: {:?}",
        response.text().await
    );
    response.json().await.expect("a profile body")
}

/// Minimal query-value encoder — the fixture's ids carry `:` and nothing else that needs it.
fn urlencoding_encode(id: &str) -> String {
    id.replace(':', "%3A")
}

/// Every paged section of the profile, keyed by its `EntityProfileDto` field name — the same
/// names the manifest uses, so a mismatch names the section rather than an index.
fn section_totals(profile: &EntityProfileDto) -> Vec<(&'static str, u64)> {
    vec![
        ("officers", profile.officers.total),
        ("addresses", profile.addresses.total),
        ("contracts", profile.contracts.total),
        ("planning_applications", profile.planning_applications.total),
        ("property", profile.property.total),
        ("osint_links", profile.osint_links.total),
        ("beneficial_owners", profile.beneficial_owners.total),
        ("disqualifications", profile.disqualifications.total),
        ("trustee_of", profile.trustee_of.total),
        ("donates_to", profile.donates_to.total),
        ("offshore_links", profile.offshore_links.total),
        ("related", profile.related.total),
    ]
}

fn status_word(profile: &EntityProfileDto) -> &'static str {
    use urban_integrity_workbench_dto::EntityStatusView as S;
    match profile.status.value {
        S::Active => "Active",
        S::Dissolved { .. } => "Dissolved",
        S::InLiquidation { .. } => "InLiquidation",
    }
}

fn assert_profile_matches(expected: &ExpectedProfile, actual: &EntityProfileDto) {
    assert_eq!(actual.id, expected.id);
    assert_eq!(actual.name, expected.name, "{}'s name", expected.id);
    assert_eq!(
        status_word(actual),
        expected.status,
        "{}'s status word",
        expected.id
    );

    let actual_sections = section_totals(actual);
    let expected_names: Vec<&str> = expected.sections.iter().map(|(n, _)| *n).collect();
    let actual_names: Vec<&str> = actual_sections.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        actual_names, expected_names,
        "the manifest must name every paged section of the profile, in order — a section added to \
         the DTO and not to the manifest would go unchecked forever"
    );
    for ((name, expected_total), (_, actual_total)) in
        expected.sections.iter().zip(actual_sections.iter())
    {
        assert_eq!(
            *actual_total as usize, *expected_total,
            "{}'s {name} section",
            expected.id
        );
    }

    // Through `pivot`, not through a bare id field: a related row that renders as plain text
    // because its target has no profile route is a different row from one that opens, and the
    // recursive pivot the checklist exercises is made entirely of the second kind. Every relative
    // in this fixture is an `Entity`, so every one of these must be `Some`.
    let mut related: Vec<&str> = actual
        .related
        .rows
        .iter()
        .map(|r| {
            r.pivot
                .as_ref()
                .unwrap_or_else(|| {
                    panic!(
                        "{}'s related row {:?} has no pivot — it would render as dead text",
                        expected.id, r.name
                    )
                })
                .id
                .as_str()
        })
        .collect();
    related.sort_unstable();
    let mut expected_related: Vec<&str> = expected.related_ids.to_vec();
    expected_related.sort_unstable();
    assert_eq!(
        related, expected_related,
        "{}'s related entities — the recursive-pivot surface",
        expected.id
    );
}

// ── The profile screen ───────────────────────────────────────────────────────

/// **Every profile expectation in the manifest is what the route returns.**
///
/// Five subjects, twelve sections each, through the real HTTP route. The two that matter most are
/// the ones a reader would call bugs: the address cluster's company, whose seven relatives are
/// reached with **no officer at all**, and the buyer of all four contracts, whose profile is
/// entirely empty because the read path counts `AwardedTo` from the `src` side and a buyer is
/// never one.
///
/// PROVE THIS CAN FAIL: change any single count in `Manifest::profiles` — e.g. the buyer's
/// `("contracts", 0)` to `("contracts", 4)`, which is the wrong answer a reader is most likely to
/// assume. Run for real.
#[tokio::test]
async fn every_planted_profile_matches_the_manifest() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr, graph) = planted_router("planted_profile").await;

    assert!(
        graph.manifest.profiles.len() >= 5,
        "non-vacuity: the manifest must carry profile expectations, found {}",
        graph.manifest.profiles.len()
    );
    for expected in &graph.manifest.profiles {
        let actual = get_profile(addr, expected.id, None).await;
        assert_profile_matches(expected, &actual);
        assert_eq!(
            actual.as_at, None,
            "an unfiltered profile must report no as-at date"
        );
    }

    // Non-vacuity: the five subjects are not five copies of the same empty profile.
    let distinct: std::collections::BTreeSet<Vec<(&str, usize)>> = graph
        .manifest
        .profiles
        .iter()
        .map(|p| p.sections.to_vec())
        .collect();
    assert!(
        distinct.len() >= 4,
        "the subjects must differ from each other, found {} distinct shapes",
        distinct.len()
    );
}

/// **An unfiltered profile claims nothing about any date.**
///
/// `AsAtStatus::NotFiltered` is a different answer from `InForce`, and the distinction is the
/// reason the enum has four variants rather than three: a screen with no filter has not
/// established that anything was in force, and a row that came back `InForce` on an unfiltered
/// load would be an unasked-for claim.
///
/// PROVE THIS CAN FAIL: have `query_entity_by_id_as_at` call `apply_as_at` with today's date when
/// `as_at` is `None`. Run for real.
#[tokio::test]
async fn an_unfiltered_profile_marks_every_row_not_filtered() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr, graph) = planted_router("planted_profile_unfiltered").await;
    let subject = graph.manifest.as_at_profiles[0].id;
    let profile = get_profile(addr, subject, None).await;

    let statuses: Vec<AsAtStatus> = profile
        .officers
        .rows
        .iter()
        .map(|r| r.temporal.status)
        .chain(profile.addresses.rows.iter().map(|r| r.temporal.status))
        .chain(profile.contracts.rows.iter().map(|r| r.temporal.status))
        .collect();
    assert!(
        !statuses.is_empty(),
        "non-vacuity: {subject} must have rows to be unfiltered about"
    );
    for status in statuses {
        assert_eq!(status, AsAtStatus::NotFiltered);
    }
}

/// **The as-at filter marks rows and removes none, and the same subject shows all three answers
/// as the date moves.**
///
/// The three tallies must sum to the unfiltered totals: PIV-04's rule is that a filter never
/// shortens a section, and that is checked here rather than assumed, because a filter that
/// silently dropped rows would still pass a test that only counted the in-force ones.
///
/// PROVE THIS CAN FAIL: change one `SectionTally` — e.g. the 2022-06-30 contract row from
/// `out_of_force: 1` to `in_force: 1`, which is the answer you get if the filter compares against
/// the wrong endpoint. Run for real.
#[tokio::test]
async fn the_as_at_tallies_match_the_manifest_and_nothing_is_removed() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr, graph) = planted_router("planted_profile_as_at").await;
    assert!(
        graph.manifest.as_at_profiles.len() >= 3,
        "non-vacuity: at least three dates, or the fixture proves nothing about movement"
    );

    let mut seen: std::collections::BTreeSet<String> = Default::default();
    for expected in &graph.manifest.as_at_profiles {
        let mut signature: Vec<(&str, usize, usize)> = Vec::new();
        let profile = get_profile(addr, expected.id, Some(expected.as_at)).await;
        assert_eq!(
            profile.as_at.as_deref(),
            Some(expected.as_at),
            "a filtered profile must report the date it was filtered at"
        );
        let unfiltered = get_profile(addr, expected.id, None).await;

        for tally in expected.sections {
            let (rows, unfiltered_total) = match tally.section {
                "officers" => (
                    profile
                        .officers
                        .rows
                        .iter()
                        .map(|r| r.temporal.status)
                        .collect::<Vec<_>>(),
                    unfiltered.officers.total,
                ),
                "addresses" => (
                    profile
                        .addresses
                        .rows
                        .iter()
                        .map(|r| r.temporal.status)
                        .collect::<Vec<_>>(),
                    unfiltered.addresses.total,
                ),
                "contracts" => (
                    profile
                        .contracts
                        .rows
                        .iter()
                        .map(|r| r.temporal.status)
                        .collect::<Vec<_>>(),
                    unfiltered.contracts.total,
                ),
                other => panic!("the manifest names a section this test cannot read: {other}"),
            };
            let count = |want: AsAtStatus| rows.iter().filter(|s| **s == want).count();
            assert_eq!(
                count(AsAtStatus::InForce),
                tally.in_force,
                "{} {} in force at {}",
                expected.id,
                tally.section,
                expected.as_at
            );
            assert_eq!(
                count(AsAtStatus::OutOfForce),
                tally.out_of_force,
                "{} {} outside at {}",
                expected.id,
                tally.section,
                expected.as_at
            );
            assert_eq!(
                count(AsAtStatus::Undated),
                tally.undated,
                "{} {} not tested at {}",
                expected.id,
                tally.section,
                expected.as_at
            );
            assert_eq!(
                rows.len() as u64,
                unfiltered_total,
                "the {} section lost rows to the filter at {} — a filter marks, it never removes",
                tally.section,
                expected.as_at
            );
            signature.push((tally.section, tally.in_force, tally.out_of_force));
        }
        seen.insert(format!("{signature:?}"));
    }

    // Non-vacuity: the three dates produce three **different screens**.
    //
    // The first version of this assertion counted distinct per-section chip sets and demanded
    // three, which cannot happen and said so on its first run: every section of this subject
    // holds exactly one row, so a section can only ever be `[InForce]` or `[OutOfForce]` and two
    // is the ceiling. The property actually worth asserting is one level up — that moving the
    // date moves the page — and it is asserted over the whole profile's signature rather than one
    // section's, because a fixture where every section flipped together would prove nothing about
    // a filter that the totals had not already proved.
    assert_eq!(
        seen.len(),
        graph.manifest.as_at_profiles.len(),
        "each date must produce a distinct screen, found {seen:?}"
    );
}

// ── Disambiguation (PIV-01) ──────────────────────────────────────────────────

/// **Every disambiguation expectation in the manifest is what the route returns, including the
/// one where the ten-match cap bites.**
///
/// PROVE THIS CAN FAIL: change `"Planted Cluster"`'s `rows: 8` to `7`. Run for real.
#[tokio::test]
async fn every_planted_disambiguation_matches_the_manifest() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr, graph) = planted_router("planted_profile_disambiguate").await;
    assert!(
        graph
            .manifest
            .disambiguations
            .iter()
            .any(|d| d.more_may_exist),
        "non-vacuity: one query must reach the cap, or the cap is never exercised"
    );

    for expected in &graph.manifest.disambiguations {
        let url = format!(
            "http://{addr}/api/entity/disambiguate?q={}",
            expected.query.replace(' ', "%20")
        );
        let view: DisambiguationView = client()
            .get(&url)
            .send()
            .await
            .expect("disambiguate request")
            .json()
            .await
            .expect("a disambiguation body");
        assert_eq!(
            view.rows.len(),
            expected.rows,
            "{:?} must match {} entities, got {:?}",
            expected.query,
            expected.rows,
            view.rows.iter().map(|r| &r.entity.id).collect::<Vec<_>>()
        );
        assert_eq!(
            view.more_may_exist, expected.more_may_exist,
            "{:?}'s cap report",
            expected.query
        );
    }
}

// ── Neighbourhood expansion (CNV-01) ─────────────────────────────────────────

/// **The expansion reaches the manifest's node set, and a cap below it is reported rather than
/// silently applied.**
///
/// Two requests: one with the cap above the total, which must return everything and still report
/// the cap it did not need; one with the cap below it, where `nodes.len() + cut == total`. The
/// arithmetic is the point — 44's neighbourhood gate found a cap that bit without saying so.
///
/// PROVE THIS CAN FAIL: change the manifest's `total: 2` to `3`. Run for real.
#[tokio::test]
async fn the_planted_neighbourhood_reports_its_cap() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr, graph) = planted_router("planted_profile_neighbourhood").await;

    for expected in &graph.manifest.neighbourhoods {
        assert!(
            expected.total >= 2,
            "non-vacuity: a cap cannot be shown to bite on a neighbourhood of one"
        );
        let get = |cap: usize| {
            let url = format!(
                "http://{addr}/api/entity/neighbourhood?id={}&hops={}&node_cap={cap}",
                urlencoding_encode(expected.id),
                expected.hops
            );
            async move {
                client()
                    .get(&url)
                    .send()
                    .await
                    .expect("neighbourhood request")
                    .json::<NeighbourhoodView>()
                    .await
                    .expect("a neighbourhood body")
            }
        };

        let whole = get(expected.total + 10).await;
        assert_eq!(whole.total_discovered as usize, expected.total);
        let mut reached: Vec<&str> = whole.nodes.iter().map(|n| n.entity.id.as_str()).collect();
        reached.sort_unstable();
        let mut wanted: Vec<&str> = expected.node_ids.to_vec();
        wanted.sort_unstable();
        assert_eq!(
            reached, wanted,
            "{}'s neighbours at {} hops",
            expected.id, expected.hops
        );
        assert!(
            !whole.truncation.truncated,
            "a cap above the total must not report a cut"
        );

        let capped = get(expected.total - 1).await;
        assert_eq!(capped.total_discovered as usize, expected.total);
        assert_eq!(capped.nodes.len(), expected.total - 1);
        assert!(
            capped.truncation.truncated,
            "a cap below the total must report a cut"
        );
        assert_eq!(
            capped.truncation.reason.as_deref(),
            Some("node_cap"),
            "the cut must name the cap that made it, not merely report that one happened"
        );
        assert_eq!(
            capped.nodes.len() + (capped.total_discovered as usize - capped.nodes.len()),
            expected.total,
            "the reported cut and the returned nodes must account for every node the walk reached"
        );
        assert!(
            capped
                .truncation
                .detail
                .contains(&expected.total.to_string()),
            "the cut's own sentence must carry the figure the analyst needs: {:?}",
            capped.truncation.detail
        );
    }
}
