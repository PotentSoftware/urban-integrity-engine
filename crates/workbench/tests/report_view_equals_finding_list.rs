//! D-15's DB-gated equality proof (43-05 Task 3): the `ReportView` built for a fixture analysis
//! is structurally equivalent to the `Finding` list the Markdown writer receives for the SAME
//! battery run — every entry lands in exactly one of `ReportView`'s 6 groups, each `ProbeView`'s
//! `VerdictKind` tag matches its source `Finding`'s, and the Skipped group's count matches exactly.
//!
//! A second `#[tokio::test]` in this file proves `last_event::compute`/`oracle::
//! max_timestamp_for_prefix`'s correctness against known seeded timestamps.
//!
//! Mirrors `crates/cli/tests/workbench_boot.rs`'s skip-by-return + `NsGuard::with_prefix`
//! pattern, and `crates/cli/tests/evidence_report_structure.rs`'s `seed_small_fixture` shape —
//! this file cannot depend on `urban-integrity-cli` (would create a `workbench -> cli` edge for
//! test-only code; simpler to hand-roll the two raw connections directly, exactly as
//! `workbench_boot.rs`'s own `register_fixture_analysis` already does for its `_catalog`
//! connection).

use surrealdb::opt::auth::Root;
use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_probe::run::{execute, OcProbeGates, RunOptions};
use urban_integrity_schema::types::edge::{
    AwardedToData, DirectsData, EdgeMeta, EdgeWeight, LocatedAtData,
};
use urban_integrity_schema::types::node::{
    ApplicantProvenance, ContractData, EntityData, NodeWeight, OfficerData,
};
use urban_integrity_schema::types::verdict::{Verdict, VerdictKind};
use urban_integrity_workbench::graph::label_index::LabelIndex;
use urban_integrity_workbench::report_view::from_battery;

// ── Fixture connection helpers — raw connects, matching `workbench_boot.rs::
// register_fixture_analysis`'s own shape (never a hand-rolled namespace name; the namespace
// itself always comes from `NsGuard::with_prefix`) ──────────────────────────────────────────

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

async fn seed_node(db: &Surreal<Any>, entity_id: &str, weight: &NodeWeight) {
    urban_integrity_test_support::events::seed_node_event(
        db,
        &format!("NodeAdded-{entity_id}"),
        entity_id,
        weight,
    )
    .await
    .expect("failed to seed node event");
}

async fn seed_edge(
    db: &Surreal<Any>,
    label: &str,
    src_id: &str,
    dst_id: &str,
    weight: &EdgeWeight,
) {
    urban_integrity_test_support::events::seed_edge_event(
        db,
        &format!("EdgeAdded-{label}-{src_id}-{dst_id}"),
        src_id,
        dst_id,
        weight,
    )
    .await
    .expect("failed to seed edge event");
}

fn edge_meta() -> EdgeMeta {
    EdgeMeta {
        created_at: chrono::Utc::now(),
        effective_from: None,
        effective_to: None,
    }
}

/// A small but non-trivial fixture — one Entity/Officer/Contract node each plus their edges,
/// copied from `crates/cli/tests/evidence_report_structure.rs::seed_small_fixture` (this file's
/// own documented discretion: same corpus shape, adapted so several probes return real,
/// non-`DataGap` verdicts rather than the trivial all-empty path).
async fn seed_small_fixture(db: &Surreal<Any>) {
    seed_node(
        db,
        "GB-COH:WB0501",
        &NodeWeight::Entity(EntityData {
            name: "Plan 43-05 Test Entity Ltd".to_string(),
            company_number: Some("WB0501".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "director:wb-43-05-director",
        &NodeWeight::Officer(OfficerData {
            name: "Plan 43-05 Test Director".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("WB0501".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "planning:WB05-C1",
        &NodeWeight::Contract(ContractData {
            name: "Plan 43-05 Test Planning Application".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("approved".to_string()),
            date_published: Some("2026-01-01".to_string()),
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: Some("WB/2026/0005".to_string()),
            application_received_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    seed_edge(
        db,
        "directs",
        "director:wb-43-05-director",
        "GB-COH:WB0501",
        &EdgeWeight::Directs {
            meta: edge_meta(),
            data: DirectsData {
                details: String::new(),
                officer_role: Some("director".to_string()),
            },
        },
    )
    .await;
    seed_edge(
        db,
        "awarded-to",
        "GB-COH:WB0501",
        "planning:WB05-C1",
        &EdgeWeight::AwardedTo {
            meta: edge_meta(),
            data: AwardedToData {
                details: String::new(),
            },
        },
    )
    .await;
    seed_edge(
        db,
        "located-at",
        "planning:WB05-C1",
        "location:wb-43-05-loc",
        &EdgeWeight::LocatedAt {
            meta: edge_meta(),
            data: LocatedAtData {
                details: String::new(),
            },
        },
    )
    .await;
}

/// D-15: the `ReportView` built for a fixture analysis is the SAME partition of the SAME
/// `Finding` list the Markdown writer receives for that run.
///
/// (a) the set of probe names across all 6 `ReportView` groups equals the set of probe names in
///     `result.entries` — every entry lands somewhere, none duplicated.
/// (b) for every entry, `ReportView`'s corresponding `ProbeView.verdict`'s `VerdictKind` tag
///     matches `VerdictKind::of(&entry.finding.verdict)`.
/// (c) the Skipped group's count equals the count of `Verdict::Skipped` entries, including the
///     zero case (`probes: []`, not an absent field).
#[tokio::test]
async fn report_view_from_battery_equals_the_finding_list_the_markdown_writer_receives() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "[skip] report_view_from_battery_equals_the_finding_list_the_markdown_writer_receives \
             (set URBAN_DB_TESTS=1 with SurrealDB running to execute)"
        );
        return;
    }

    let guard = urban_integrity_test_support::NsGuard::with_prefix("test_report_view");
    let db = connect_analysis_db(guard.name(), "main").await;
    let catalog = connect_catalog_db(guard.name()).await;

    seed_small_fixture(&db).await;

    let result = execute(
        &db,
        catalog,
        "main".to_string(),
        OcProbeGates::default(),
        RunOptions { full_battery: true },
    )
    .await
    .expect("full-battery execute() must succeed against the seeded fixture");

    let run_at = chrono::Utc::now();
    // An empty label index: this test is about the entries partition, not about node classes, and
    // `entity_ref_from_record_ref` falls back to `RecordKind::name` for an id the index does not
    // hold — the behaviour this call had before plan 45-11 threaded the index through.
    let view = from_battery(&result, run_at, &LabelIndex::default());
    let rendered_markdown =
        urban_integrity_probe::report::render_evidence_report(&result.entries, &result.strategy);
    // The Markdown writer received exactly `result.entries` — assert its own ledger names every
    // probe, so this test's fixture and the Markdown call are provably driven by the SAME list.
    for entry in &result.entries {
        assert!(
            rendered_markdown.contains(&entry.finding.probe),
            "Markdown ledger must name probe {} — the writer must have received it",
            entry.finding.probe
        );
    }

    assert_eq!(view.groups.len(), 6, "expected exactly 6 ReportView groups");

    // (a) exactly-one-home proof: every probe name from `result.entries` appears in EXACTLY one
    // `ReportView` group, and no group contains a probe name absent from `result.entries`.
    let entry_names: std::collections::HashSet<&str> = result
        .entries
        .iter()
        .map(|e| e.finding.probe.as_str())
        .collect();
    let mut view_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for group in &view.groups {
        for probe in &group.probes {
            assert!(
                view_names.insert(probe.name.as_str()),
                "probe {} appears in more than one ReportView group",
                probe.name
            );
        }
    }
    assert_eq!(
        view_names, entry_names,
        "the set of probe names across all 6 ReportView groups must equal the set of probe \
         names in result.entries — every entry must land somewhere, none duplicated, none \
         fabricated"
    );

    // (b) VerdictKind tag equality, per entry.
    for entry in &result.entries {
        let probe_view = view
            .groups
            .iter()
            .flat_map(|g| &g.probes)
            .find(|p| p.name == entry.finding.probe)
            .unwrap_or_else(|| panic!("probe {} missing from ReportView", entry.finding.probe));
        let wire_kind = serde_json::to_value(&probe_view.verdict)
            .expect("VerdictView must serialize")
            .get("kind")
            .and_then(|k| k.as_str())
            .map(str::to_string)
            .expect("VerdictView JSON must carry a `kind` tag");
        let expected_kind = VerdictKind::of(&entry.finding.verdict).to_string();
        assert_eq!(
            wire_kind, expected_kind,
            "probe {}'s ReportView verdict kind must match its source Finding's VerdictKind",
            entry.finding.probe
        );
    }

    // (c) Skipped group count matches exactly, including the zero case (an ungated
    // full_battery=true run produces zero Skipped entries — the explicit `probes: []` case).
    let expected_skipped = result
        .entries
        .iter()
        .filter(|e| matches!(e.finding.verdict, Verdict::Skipped { .. }))
        .count();
    let skipped_group = view
        .groups
        .iter()
        .find(|g| g.name == "Skipped")
        .expect("Skipped group must always be present");
    assert_eq!(
        skipped_group.probes.len(),
        expected_skipped,
        "Skipped group's probe count must equal the count of Verdict::Skipped entries"
    );
    if expected_skipped == 0 {
        assert!(
            skipped_group.probes.is_empty(),
            "zero-Skipped case must render as an explicit empty Vec, not an absent field"
        );
    }
}

/// The DB-gated correctness proof for `oracle::max_timestamp_for_prefix`/`last_event::compute`
/// (43-05 Task 1/3): seeds `graph_events` rows with known `payload.entity_id` values across the
/// three Home page ingestor prefixes and known `timestamp`s, and asserts `last_event::compute`
/// returns the known max per source and the correct overall.
#[tokio::test]
async fn last_event_compute_returns_the_known_max_per_source_and_overall() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "[skip] last_event_compute_returns_the_known_max_per_source_and_overall (set \
             URBAN_DB_TESTS=1 with SurrealDB running to execute)"
        );
        return;
    }

    let guard = urban_integrity_test_support::NsGuard::with_prefix("test_last_event");
    let db = connect_analysis_db(guard.name(), "main").await;

    async fn seed_at(db: &Surreal<Any>, event_id: &str, kind: &str, entity_id: &str, ts: u64) {
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": ts,
                    "kind": kind,
                    "payload": { "entity_id": entity_id, "weight_json": "{}" },
                }),
            ))
            .await
            .expect("failed to seed timestamped node event")
            .check()
            .expect("timestamped node event returned a statement error");
    }

    // GB-COH: max = 1_700_000_000_300.
    seed_at(&db, "coh-1", "NodeAdded", "GB-COH:1", 1_700_000_000_100).await;
    seed_at(&db, "coh-2", "NodeAdded", "GB-COH:1", 1_700_000_000_300).await;
    // CF: max = 1_700_000_000_999 (the overall max across all three sources).
    seed_at(&db, "cf-1", "NodeAdded", "CF:1", 1_700_000_000_200).await;
    seed_at(&db, "cf-2", "NodeUpdated", "CF:1", 1_700_000_000_999).await;
    // planning: max = 1_700_000_000_050.
    seed_at(
        &db,
        "planning-1",
        "NodeAdded",
        "planning:1",
        1_700_000_000_050,
    )
    .await;

    let summary = urban_integrity_workbench::last_event::compute(&db).await;

    assert_eq!(
        summary.companies_house,
        chrono::DateTime::from_timestamp_millis(1_700_000_000_300)
    );
    assert_eq!(
        summary.contracts_finder,
        chrono::DateTime::from_timestamp_millis(1_700_000_000_999)
    );
    assert_eq!(
        summary.planning,
        chrono::DateTime::from_timestamp_millis(1_700_000_000_050)
    );
    assert_eq!(
        summary.overall,
        chrono::DateTime::from_timestamp_millis(1_700_000_000_999),
        "overall must be the max across all three sources"
    );
}
