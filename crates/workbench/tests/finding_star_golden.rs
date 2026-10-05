//! D-11's "server golden" (Phase 50, plan 08, criterion 2): a DB-gated proof that the REAL
//! report-assembly pipeline (`execute()` + `from_battery()`, the same functions
//! `report_handler` calls) carries `address-cluster-exact`'s `MotifKind::Star` classification
//! and its exact centre+8-leaf `RecordRef`s through to `ProbeView`, matching the planted
//! manifest's fixed answer (`location:planted-l2` + the 8 `L2_COMPANIES`).
//!
//! Deliberately DB-gated, not the no-DB `workbench_from_graph` CSR-only pattern
//! (`planted_golden.rs`) — the probe battery reads `graph_events` through SurrealDB and this
//! project's SurrealDB SDK has no embedded/in-memory engine compiled in (only
//! `protocol-ws`/`protocol-http`, per `Cargo.toml`), so a no-DB golden could not actually prove
//! the probe's SurrealDB-sourced refs survive the report pipeline.
//!
//! Harness shape copied from `report_view_equals_finding_list.rs` (raw connect + schema init,
//! `execute()` -> `from_battery()`); fixture seeding copied from
//! `crates/probe/tests/planted_evidence_refs.rs`'s `seeded_fixture()` (re-implemented locally —
//! a test-only helper in another crate's `tests/` directory cannot be imported).
//!
//! `address-cluster-exact`'s evidence shape (verified live against the seeded fixture, not
//! assumed): `evidence[0]` is an aggregate row with no refs; `evidence[1..]` is one row per
//! shared-address cluster, biggest first. The planted fixture seeds TWO shared-address clusters
//! — `L2` (8 applicants, structure 2, the intended star) and `L1` (2 applicants, structure 1's
//! phoenix-loop predecessor/successor, an incidental side effect of both sharing a registered
//! address) — so this test locates the star's evidence row by `refs[0].id == L2` rather than by
//! a fixed array index, and asserts on that row specifically rather than on `evidence[0]` or a
//! sum across every row.

use surrealdb::opt::auth::Root;
use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_probe::run::{execute, OcProbeGates, RunOptions};
use urban_integrity_test_support::planted::{planted_graph, L2, L2_COMPANIES};
use urban_integrity_workbench::graph::label_index::LabelIndex;
use urban_integrity_workbench::report_view::from_battery;

async fn connect_analysis_db(ns: &str, db_name: &str) -> Surreal<Any> {
    let (username, password) = urban_integrity_test_support::credentials();
    let db = surrealdb::engine::any::connect(urban_integrity_test_support::endpoint())
        .await
        .expect("SurrealDB unreachable — start it and set URBAN_DB_TESTS=1");
    db.signin(Root { username, password })
        .await
        .expect("SurrealDB root sign-in failed");
    db.use_ns(ns)
        .use_db(db_name)
        .await
        .expect("SurrealDB namespace/database selection failed");
    db.query(urban_integrity_schema::db::SCHEMA_DEF)
        .await
        .expect("schema init failed")
        .check()
        .expect("schema init returned a statement error");
    db
}

async fn connect_catalog_db(ns: &str) -> Surreal<Any> {
    let (username, password) = urban_integrity_test_support::credentials();
    let catalog = surrealdb::engine::any::connect(urban_integrity_test_support::endpoint())
        .await
        .expect("SurrealDB unreachable — start it and set URBAN_DB_TESTS=1");
    catalog
        .signin(Root { username, password })
        .await
        .expect("SurrealDB root sign-in failed");
    catalog
        .use_ns(ns)
        .use_db("_catalog")
        .await
        .expect("select the test namespace's _catalog");
    catalog
        .query(urban_integrity_schema::db::CATALOG_SCHEMA)
        .await
        .expect("apply CATALOG_SCHEMA")
        .check()
        .expect("CATALOG_SCHEMA apply returned a statement error");
    catalog
}

/// D-11's server golden: the full `execute()` -> `from_battery()` pipeline, against the planted
/// fixture, produces a `ProbeView` for `address-cluster-exact` whose `motif_kind == "Star"` and
/// whose star-cluster evidence row carries exactly `L2` (centre) + the 8 `L2_COMPANIES` (leaves).
#[tokio::test]
async fn address_cluster_exact_star_matches_the_planted_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "[skip] address_cluster_exact_star_matches_the_planted_manifest (set \
             URBAN_DB_TESTS=1 with SurrealDB running to execute)"
        );
        return;
    }

    let guard =
        urban_integrity_test_support::NsGuard::with_prefix("test_workbench_finding_star_golden");
    let db = connect_analysis_db(guard.name(), "main").await;
    let catalog = connect_catalog_db(guard.name()).await;

    let g = planted_graph();
    urban_integrity_test_support::events::seed_planted(&db, &g)
        .await
        .expect("seed planted graph");

    let result = execute(
        &db,
        catalog,
        "main".to_string(),
        OcProbeGates::default(),
        RunOptions { full_battery: true },
    )
    .await
    .expect("full-battery execute() must succeed against the seeded planted fixture");

    let run_at = chrono::Utc::now();
    let view = from_battery(&result, run_at, &LabelIndex::default());

    let probe_view = view
        .groups
        .iter()
        .flat_map(|g| &g.probes)
        .find(|p| p.name == "address-cluster-exact")
        .expect("address-cluster-exact must appear in exactly one ReportView group");

    assert_eq!(
        probe_view.motif_kind, "Star",
        "address-cluster-exact's ProbeView.motif_kind must be \"Star\" (D-06's pinned \
         classification); got {:?}",
        probe_view.motif_kind
    );

    let star_row = probe_view
        .evidence
        .iter()
        .find(|row| row.refs.first().map(|r| r.id.as_str()) == Some(L2))
        .unwrap_or_else(|| {
            panic!(
                "no evidence row with refs[0].id == {L2} found; probe_view.evidence = {:#?}",
                probe_view.evidence
            )
        });

    assert_eq!(
        star_row.refs.len(),
        9,
        "the L2 star-cluster evidence row must carry exactly 9 refs (1 centre + 8 leaves); got \
         {:?}",
        star_row.refs
    );
    assert_eq!(
        star_row.refs[0].id, L2,
        "refs[0] must be the centre location {L2}"
    );

    let leaves: std::collections::HashSet<&str> =
        star_row.refs[1..].iter().map(|r| r.id.as_str()).collect();
    let expected_leaves: std::collections::HashSet<&str> = L2_COMPANIES.iter().copied().collect();
    assert_eq!(
        leaves, expected_leaves,
        "refs[1..] must be exactly the set of 8 L2_COMPANIES, no more, no fewer"
    );
}
