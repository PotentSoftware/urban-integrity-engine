//! PIV-02: the findings-by-entity reverse index, and the empty list that means two opposite
//! things.
//!
//! # The failure this file exists to catch
//!
//! Most of the probe battery cannot name an entity. Re-verified against the battery as it stands
//! on 2026-09-15 — not taken from plan 44-21, which measured a different set:
//!
//! | bucket | probes |
//! |---|---|
//! | names graph nodes | **6 of 22** — `address-cluster-exact`, `temporal-burst`, `ward-concentration`, `gazette-insolvency-signals`, `osint-disqualified-directors`, `osint-risk-edges` |
//! | names identity links only | **2 of 22** — `osint-confirmed-intel`, `osint-analyst-queue` |
//! | aggregate-only | **14 of 22** |
//!
//! So for most subjects this section comes back empty. An analyst reads an empty findings list as
//! *"this entity appears in no findings"*. The truth is nearly always *"no probe in this battery
//! was capable of naming it"*. Those are opposite conclusions rendered identically, and nothing
//! errors in either case.
//!
//! `a_subject_with_no_findings_is_distinguishable_from_one_that_could_not_be_searched` is the only
//! test here that matters. The rest guard the mechanics that make it true.
//!
//! # Why a reverse-index test is the easiest place in this phase to pass vacuously
//!
//! With 14 of 22 probes emitting no refs at all, a test that walks the index and finds nothing
//! looks exactly like a working one. Every test below that compares two populations
//! **counts both before comparing** and asserts each is non-empty, with a message naming the arm
//! that would otherwise be untested. `db_a_seeded_subject_resolves_through_a_real_record_ref`
//! seeds a subject that genuinely resolves, so the index is proved to return a row and not merely
//! to return nothing without crashing.
//!
//! # What this plan did NOT do
//!
//! It adds no `RecordRef` to any probe (plan 45-05's D-05). Doing so would mean inventing node ids
//! the graph does not contain, which is what 44-21 declined to do; it belongs to phase 49's
//! evidence model. The 14 aggregate-only probes are therefore still aggregate-only, and this file
//! asserts the section says so rather than pretending otherwise.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test findings_by_entity
//! ```
//!
//! The pure-function tests need no database. The `db_`-prefixed ones are gated on
//! `URBAN_DB_TESTS=1` and skip by returning, so a gated run reports the same passing count as an
//! ungated one and duration is the only witness a database was reached. Namespaces come from
//! `urban-integrity-test-support`, which drops them.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    alerts_naming_entity, findings_naming_entity, query_findings_naming_entity, EvidenceScope,
    FindingOrigin, FindingsOutcome, ProbeCoverage, Provenance,
};
use urban_integrity_schema::types::attribution;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};

const SUBJECT: &str = "GB-COH:X9995001";
const SUBJECT_CH: &str = "X9995001";
const OTHER: &str = "GB-COH:X9995002";

/// A coverage in the live shape: one searchable probe, one link-only, one aggregate-only.
///
/// Deliberately not `ProbeCoverage::default()` — an empty coverage makes every probe an unknown
/// producer, which is a different code path and would let these tests pass while the real one was
/// broken.
fn coverage() -> ProbeCoverage {
    ProbeCoverage {
        names_nodes: vec!["names-nodes-probe".to_owned()],
        names_links_only: vec!["links-only-probe".to_owned()],
        aggregate_only: vec!["aggregate-probe".to_owned()],
    }
}

fn finding(probe: &str, evidence: Vec<EvidenceRow>) -> Finding {
    Finding {
        probe: probe.to_owned(),
        verdict: Verdict::classify(1, 1, 1, 1, ""),
        summary: format!("{probe} summary"),
        cause: None,
        severity: Severity::High,
        evidence,
        attributions: vec![],
        remediation_hint: None,
    }
}

fn row(text: &str, refs: Vec<RecordRef>) -> EvidenceRow {
    EvidenceRow {
        text: text.to_owned(),
        refs,
    }
}

fn entity_ref(id: &str) -> RecordRef {
    RecordRef {
        kind: RecordKind::Entity,
        id: id.to_owned(),
    }
}

// ── The distinction the whole section exists for ────────────────────────────

/// **The one requirement.** "No probe found anything about this entity" and "no probe could have
/// found anything about this entity" must be different values in the returned data, not two
/// renderings of the same empty `Vec`.
///
/// Both calls below return `rows: []`. If the section could only say that, an analyst reading the
/// screen would draw the opposite conclusion in one of the two cases, with nothing on the page to
/// warn them.
///
/// PROVE THIS CAN FAIL: make `findings_naming_entity` derive `outcome` from `rows.is_empty()`
/// alone (collapse the `NotNamed`/`NotSearched` arms into one) and this test fails on the
/// inequality. That mutation was performed; see the plan summary.
#[test]
fn a_subject_with_no_findings_is_distinguishable_from_one_that_could_not_be_searched() {
    let cov = coverage();

    // (a) A searchable probe ran and named somebody else.
    let searched = [finding(
        "names-nodes-probe",
        vec![row("2 applicants at LOC:X", vec![entity_ref(OTHER)])],
    )];
    let not_named = findings_naming_entity(SUBJECT, Some(SUBJECT_CH), &searched, &[], &cov, None);

    // (b) Only aggregate-only and link-only probes ran. Nothing here could have named anyone.
    let unsearchable = [
        finding("aggregate-probe", vec![row("41,905 nodes scanned", vec![])]),
        finding(
            "links-only-probe",
            vec![row(
                "0.910  A  ->  B  [ch_psc]",
                vec![RecordRef {
                    kind: RecordKind::IdentityLink,
                    id: "identity_links:abc".to_owned(),
                }],
            )],
        ),
    ];
    let not_searched =
        findings_naming_entity(SUBJECT, Some(SUBJECT_CH), &unsearchable, &[], &cov, None);

    // Non-vacuity: both arms must actually be populated, or the comparison below is between two
    // things that are empty for the same uninteresting reason.
    assert_eq!(
        searched.len(),
        1,
        "the NotNamed arm needs a searchable finding, or it degenerates into the NotSearched arm"
    );
    assert_eq!(
        unsearchable.len(),
        2,
        "the NotSearched arm needs unsearchable findings, or it proves nothing about them"
    );

    assert!(not_named.rows.is_empty(), "arm (a) must return no rows");
    assert!(not_searched.rows.is_empty(), "arm (b) must return no rows");

    assert_ne!(
        not_named.outcome, not_searched.outcome,
        "an empty list that means 'no findings' and one that means 'nothing could be searched' \
         must not be the same value — they are opposite conclusions rendered identically"
    );
    assert_eq!(not_named.outcome, FindingsOutcome::NotNamed);
    assert_eq!(not_searched.outcome, FindingsOutcome::NotSearched);

    // And the counts that let a renderer say which is which.
    assert_eq!(not_named.searchable_findings, 1);
    assert_eq!(not_named.unsearchable_findings, 0);
    assert_eq!(not_searched.searchable_findings, 0);
    assert_eq!(
        not_searched.unsearchable_findings, 2,
        "the section has to be able to say how many findings it was silent about"
    );
}

/// The coverage figure is carried on the section, not in a comment or a log line — and it says
/// the three numbers a reader needs.
///
/// PROVE THIS CAN FAIL: drop `aggregate_only` from `probes_total()` and the total no longer equals
/// the sum of the buckets; the statement then reports "Searched 1 of 2 probes" and the substring
/// assertion fails.
#[test]
fn the_section_states_its_own_coverage_rather_than_leaving_it_to_a_comment() {
    let cov = coverage();
    let out = findings_naming_entity(SUBJECT, None, &[], &[], &cov, None);

    assert_eq!(out.coverage.probes_total(), 3);
    assert_eq!(out.coverage.probes_searchable(), 1);
    assert_eq!(out.coverage.probes_not_searchable(), 2);
    assert_eq!(
        out.coverage.probes_searchable() + out.coverage.probes_not_searchable(),
        out.coverage.probes_total(),
        "every probe must be in exactly one bucket, or the stated coverage is unfalsifiable"
    );

    let statement = out.coverage.statement();
    assert!(
        statement.contains("Searched 1 of 3 probes"),
        "the statement must carry the figures, not a vague sentence: {statement}"
    );
    assert!(
        statement.contains("An empty list here means no searchable probe named this entity"),
        "the statement must say what an empty list does NOT mean: {statement}"
    );
}

// ── Matching ────────────────────────────────────────────────────────────────

/// An `Entity` ref matches on the node id exactly, and a ref naming a different subject does not.
///
/// Asserts its own complement: a test that only checked the positive case would pass against a
/// matcher that returned `true` for everything, which is the shape that would put every finding in
/// the corpus on every profile.
///
/// PROVE THIS CAN FAIL: make `ref_names_entity`'s `Entity` arm return `true` unconditionally and
/// the `OTHER` assertion fails.
#[test]
fn an_entity_ref_matches_its_own_id_and_not_another() {
    let cov = coverage();
    let findings = [finding(
        "names-nodes-probe",
        vec![
            row("names the subject", vec![entity_ref(SUBJECT)]),
            row("names somebody else", vec![entity_ref(OTHER)]),
        ],
    )];

    let out = findings_naming_entity(SUBJECT, Some(SUBJECT_CH), &findings, &[], &cov, None);
    assert_eq!(out.rows.len(), 1, "exactly the row naming the subject");
    assert_eq!(out.rows[0].evidence_text, "names the subject");
    assert_eq!(out.rows[0].matched_id, SUBJECT);
    assert_eq!(out.outcome, FindingsOutcome::Named);

    let other = findings_naming_entity(OTHER, None, &findings, &[], &cov, None);
    assert_eq!(other.rows.len(), 1, "and the complement, for the other id");
    assert_eq!(other.rows[0].evidence_text, "names somebody else");
}

/// A `CompanyNumber` ref matches through either spelling — the probe holds `X9995001`, the profile
/// holds `GB-COH:X9995001` — and an `IdentityLink` ref matches nothing, which is the refusal
/// `EvidenceScope::NamesLinksOnly` exists to report.
///
/// Both halves in one test because the point is the contrast: two ref kinds, one followed and one
/// deliberately not.
///
/// PROVE THIS CAN FAIL: give the `IdentityLink` arm the same `r.id == entity_id` body as `Entity`
/// and the link assertion fails; drop the `GB-COH:` fallback and the bare-number assertion fails.
#[test]
fn a_company_number_ref_matches_both_spellings_and_a_link_ref_matches_nothing() {
    let cov = coverage();
    let findings = [finding(
        "names-nodes-probe",
        vec![
            row(
                "by bare company number",
                vec![RecordRef {
                    kind: RecordKind::CompanyNumber,
                    id: SUBJECT_CH.to_owned(),
                }],
            ),
            row(
                "by identity link",
                vec![RecordRef {
                    kind: RecordKind::IdentityLink,
                    id: SUBJECT.to_owned(),
                }],
            ),
        ],
    )];

    // With no `ch_number` in hand, the `GB-COH:` id itself still answers.
    let without_ch = findings_naming_entity(SUBJECT, None, &findings, &[], &cov, None);
    assert_eq!(
        without_ch.rows.len(),
        1,
        "the GB-COH: id must answer a bare-number ref even when no ch_number was passed"
    );
    assert_eq!(without_ch.rows[0].named_by, RecordKind::CompanyNumber);

    // And with it, for a subject whose node id is not `GB-COH:`-shaped.
    let by_ch = findings_naming_entity(
        "director:someone",
        Some(SUBJECT_CH),
        &findings,
        &[],
        &cov,
        None,
    );
    assert_eq!(by_ch.rows.len(), 1);

    // The link ref names the same string and is still not followed.
    assert!(
        without_ch
            .rows
            .iter()
            .all(|r| r.named_by != RecordKind::IdentityLink),
        "an identity-link id is not a node id; following it needs an identity_links read this \
         index does not do, and pretending otherwise would put a link id in a node pivot"
    );
}

// ── Provenance and producers ────────────────────────────────────────────────

/// A probe finding is attributed to `probe`, never to `detector`, and never stamped with the time
/// the section was assembled.
///
/// The second half is plan 45-04's rule: a missing timestamp is `Unattributed` with a reason, not
/// a plausible-looking one. `Utc::now()` here would render as a provenance chip claiming the
/// finding was observed at page-load.
///
/// PROVE THIS CAN FAIL: have `probe_provenance`'s `None` arm return
/// `Attributed { observed_at: Utc::now() }` and the `Unattributed` assertion fails; set
/// `source_dataset` to `attribution::DETECTOR` and the producer assertion fails.
#[test]
fn a_probe_finding_is_attributed_to_the_probe_and_an_unrecorded_run_time_is_not_invented() {
    let cov = coverage();
    let findings = [finding(
        "names-nodes-probe",
        vec![row("names the subject", vec![entity_ref(SUBJECT)])],
    )];

    let dated = findings_naming_entity(
        SUBJECT,
        None,
        &findings,
        &[],
        &cov,
        Some("2026-09-15T10:11:00Z"),
    );
    match &dated.rows[0].provenance {
        Provenance::Attributed {
            source_dataset,
            observed_at,
            tier,
        } => {
            assert_eq!(source_dataset, attribution::PROBE);
            assert_ne!(
                source_dataset,
                attribution::DETECTOR,
                "a probe and the motif detector share a chip column; one token for both names the \
                 wrong producer on whichever row it is wrong for"
            );
            assert_eq!(observed_at, "2026-09-15T10:11:00Z");
            assert!(
                tier.is_none(),
                "a finding is not derived from an identity link"
            );
        }
        other => panic!("a finding from a timed run must be attributed, got {other:?}"),
    }

    let undated = findings_naming_entity(SUBJECT, None, &findings, &[], &cov, None);
    match &undated.rows[0].provenance {
        Provenance::Unattributed { reason } => assert!(
            reason.contains("did not record when it ran"),
            "the reason must say what was missing, got {reason:?}"
        ),
        other => panic!("an untimed run must not be stamped with a plausible time, got {other:?}"),
    }
}

/// A finding from a probe the coverage has never heard of is **still searched**, and is counted in
/// neither tally — so a stale registry appears as a third number instead of corrupting the other
/// two.
///
/// PROVE THIS CAN FAIL: count unknown producers as `unsearchable_findings` and the
/// `unsearchable_findings == 0` assertion fails, which is the version where a stale registry makes
/// the section under-report its own reach.
#[test]
fn a_finding_from_an_unregistered_probe_is_searched_and_counted_separately() {
    let cov = coverage();
    let findings = [finding(
        "probe-added-after-the-registry-was-written",
        vec![row("names the subject", vec![entity_ref(SUBJECT)])],
    )];

    let out = findings_naming_entity(SUBJECT, None, &findings, &[], &cov, None);

    assert_eq!(
        out.rows.len(),
        1,
        "refusing to search an unregistered probe would hide a real row behind bookkeeping"
    );
    assert_eq!(
        out.unknown_producers,
        vec!["probe-added-after-the-registry-was-written".to_owned()],
        "and the staleness must be visible"
    );
    assert_eq!(out.searchable_findings, 0);
    assert_eq!(out.unsearchable_findings, 0);
}

/// `EvidenceScope` has three variants and exactly one of them is searchable. A two-variant version
/// would put `NamesLinksOnly` on one side or the other, and both are wrong: counted as searchable
/// it inflates the coverage figure with probes that can never produce a row; counted as
/// aggregate-only it reports "no id in hand" about probes that hold one.
#[test]
fn only_node_naming_evidence_is_searchable() {
    assert!(EvidenceScope::NamesNodes.is_searchable_by_node_id());
    assert!(!EvidenceScope::NamesLinksOnly.is_searchable_by_node_id());
    assert!(!EvidenceScope::AggregateOnly.is_searchable_by_node_id());
}

/// `scope_of` answers from the bucket a probe is actually in, and `None` for one in no bucket.
#[test]
fn scope_of_reads_the_bucket_and_admits_when_a_probe_is_in_none() {
    let cov = coverage();
    assert_eq!(
        cov.scope_of("names-nodes-probe"),
        Some(EvidenceScope::NamesNodes)
    );
    assert_eq!(
        cov.scope_of("links-only-probe"),
        Some(EvidenceScope::NamesLinksOnly)
    );
    assert_eq!(
        cov.scope_of("aggregate-probe"),
        Some(EvidenceScope::AggregateOnly)
    );
    assert_eq!(cov.scope_of("never-registered"), None);
}

// ── Alerts ─────────────────────────────────────────────────────────────────

/// An alert naming the entity is a row in this section too, and it keeps the detector's own
/// provenance rather than being re-attributed to `probe`.
///
/// This is what stops the section being empty for the subjects that matter most: a `PhoenixLoop`
/// alert names its participants, so an entity with an alert always has at least one row even
/// though 14 of 22 probes could never name it.
///
/// PROVE THIS CAN FAIL: attribute the alert row with `probe_provenance` and the `detector`
/// assertion fails.
#[test]
fn an_alert_row_keeps_the_detector_as_its_producer() {
    let cov = coverage();
    let alerts = vec![alert_record("alert-1", "PhoenixLoop", 0.85)];

    let out = findings_naming_entity(
        SUBJECT,
        None,
        &[],
        &alerts,
        &cov,
        Some("2026-09-15T00:00:00Z"),
    );

    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.outcome, FindingsOutcome::Named);
    match &out.rows[0].origin {
        FindingOrigin::MotifAlert { motif_type, score } => {
            assert_eq!(motif_type, "PhoenixLoop");
            assert!((score - 0.85).abs() < f64::EPSILON);
        }
        other => panic!("an alert must arrive as a MotifAlert origin, got {other:?}"),
    }
    match &out.rows[0].provenance {
        Provenance::Attributed { source_dataset, .. } => assert_eq!(
            source_dataset,
            attribution::DETECTOR,
            "an alert is the detector's assertion, not the probe battery's"
        ),
        other => panic!("expected the detector's provenance, got {other:?}"),
    }
}

fn alert_record(
    id: &str,
    motif_type: &str,
    score: f64,
) -> urban_integrity_core::persistence::queries::AlertRecord {
    urban_integrity_core::persistence::queries::AlertRecord {
        id: id.to_owned(),
        motif_type: motif_type.to_owned(),
        score,
        result_json: "{}".to_owned(),
        detected_at: "2026-08-11T00:00:00Z".to_owned(),
        ward_code: None,
        withdrawn_at: None,
        withdrawn_reason: None,
        participants: vec![],
        first_seen_run_id: None,
        last_seen_run_id: None,
        provenance: Provenance::Attributed {
            source_dataset: attribution::DETECTOR.to_owned(),
            observed_at: "2026-08-11T00:00:00Z".to_owned(),
            tier: None,
        },
    }
}

/// The order is total: same inputs, same section, every time. A partial order would make the
/// output depend on the order the battery happened to run in.
#[test]
fn the_row_order_is_deterministic() {
    let cov = coverage();
    let findings = [finding(
        "names-nodes-probe",
        vec![
            row("zeta", vec![entity_ref(SUBJECT)]),
            row("alpha", vec![entity_ref(SUBJECT)]),
            row("mu", vec![entity_ref(SUBJECT)]),
        ],
    )];
    let alerts = vec![
        alert_record("a-low", "AddressCluster", 0.40),
        alert_record("a-high", "PhoenixLoop", 0.85),
    ];

    let first = findings_naming_entity(SUBJECT, None, &findings, &alerts, &cov, None);
    let again = findings_naming_entity(SUBJECT, None, &findings, &alerts, &cov, None);

    assert_eq!(first.rows.len(), 5, "3 evidence rows plus 2 alerts");
    assert_eq!(
        first.rows, again.rows,
        "the order must not vary between calls"
    );

    let texts: Vec<&str> = first
        .rows
        .iter()
        .map(|r| r.evidence_text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec!["a-high", "a-low", "alpha", "mu", "zeta"],
        "alerts worst-first, then findings by evidence text"
    );
}

// ── DB-gated ───────────────────────────────────────────────────────────────

fn base_timestamp() -> u64 {
    1_760_000_000_000
}

fn motif_row(
    event_id: &str,
    motif_type: &str,
    score: f64,
    participants: &[&str],
) -> serde_json::Value {
    let result = serde_json::json!({
        "motif_type": motif_type,
        "risk_score": score,
        "score_breakdown": { "ward_code": "X05999001" },
        "participants": participants,
        "unique_hash": event_id,
    });
    serde_json::json!({
        "event_id": event_id,
        "timestamp": base_timestamp(),
        "kind": "MotifDetected",
        "payload": { "result_json": result.to_string() },
    })
}

/// A withdrawn alert: the flag lives inside `result_json`, which is where `parse_alert_row` reads
/// it from.
fn withdrawn_motif_row(
    event_id: &str,
    motif_type: &str,
    score: f64,
    participant: &str,
) -> serde_json::Value {
    let result = serde_json::json!({
        "motif_type": motif_type,
        "risk_score": score,
        "score_breakdown": { "ward_code": "X05999001" },
        "participants": [participant],
        "unique_hash": event_id,
        "withdrawn_at": "2026-09-01T00:00:00Z",
        "withdrawn_reason": "seeded retraction",
    });
    serde_json::json!({
        "event_id": event_id,
        "timestamp": base_timestamp(),
        "kind": "MotifDetected",
        "payload": { "result_json": result.to_string() },
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
/// second opinion on "did the batched INSERT land". Without it every assertion below would be
/// comparing the read path against a fiction.
async fn count_events(db: &Surreal<Any>) -> usize {
    let n: Option<i64> = db
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .expect("count query")
        .take((0, "count"))
        .expect("count take");
    n.unwrap_or(0) as usize
}

/// **Non-vacuity, seeded.** A subject that genuinely resolves through a `RecordRef`, so the index
/// is proved to return a row rather than proved not to crash while returning nothing.
///
/// Seeded rather than found: on `an_anglesey_aebaa785` the whole battery produces 1,844 node-naming
/// pivots across 1,844 distinct ids, of which 1,071 are `GB-COH:` companies — 1.45% of the 73,865
/// profilable subjects. A test that picked a live subject at random would be empty 98.5% of the
/// time and would look exactly like a working one.
///
/// PROVE THIS CAN FAIL: make `alerts_naming_entity` return `Ok(vec![])` and the alert assertions
/// fail; make `ref_names_entity` return `false` for `Entity` and the finding assertion fails.
#[tokio::test]
async fn db_a_seeded_subject_resolves_through_a_real_record_ref() {
    if !db_tests_enabled() {
        eprintln!("skipping: URBAN_DB_TESTS not set");
        return;
    }
    let db = TestNs::connect("findings_by_entity_resolves").await;

    let rows = vec![
        motif_row("x9995-alert-live", "PhoenixLoop", 0.85, &[SUBJECT, OTHER]),
        motif_row("x9995-alert-other", "AddressCluster", 0.40, &[OTHER]),
        withdrawn_motif_row("x9995-alert-withdrawn", "PhoenixLoop", 0.99, SUBJECT),
    ];
    let seeded = rows.len();
    insert_rows(&db, rows).await;
    assert_eq!(
        count_events(&db).await,
        seeded,
        "the database's own count must agree with the seeder, or every assertion below compares \
         the read path against a fiction"
    );

    // The DB half.
    let alerts = alerts_naming_entity(&db, SUBJECT)
        .await
        .expect("alerts read");
    assert_eq!(
        alerts.len(),
        1,
        "exactly the live alert naming the subject: the other-subject alert must not match, and \
         the withdrawn one must not be volunteered (T-23-FalseAccusation)"
    );
    assert_eq!(alerts[0].id, "x9995-alert-live");

    // Both populations counted before either is compared.
    let other_alerts = alerts_naming_entity(&db, OTHER).await.expect("alerts read");
    assert_eq!(
        other_alerts.len(),
        2,
        "the complement: OTHER is named by both live alerts, so a matcher returning a constant \
         cannot pass both this and the assertion above"
    );

    // The whole section, through the joined read path.
    let cov = coverage();
    let findings = [finding(
        "names-nodes-probe",
        vec![row("2 applicants at LOC:X9995", vec![entity_ref(SUBJECT)])],
    )];
    let section = query_findings_naming_entity(
        &db,
        SUBJECT,
        Some(SUBJECT_CH),
        &findings,
        &cov,
        Some("2026-09-15T10:11:00Z"),
    )
    .await
    .expect("section read");

    assert_eq!(section.outcome, FindingsOutcome::Named);
    assert_eq!(
        section.rows.len(),
        2,
        "one probe finding and one alert — a section with only one of them would pass a test that \
         merely checked it was non-empty"
    );
    let producers: Vec<bool> = section
        .rows
        .iter()
        .map(|r| matches!(r.origin, FindingOrigin::MotifAlert { .. }))
        .collect();
    assert!(
        producers.contains(&true) && producers.contains(&false),
        "both producer kinds must be present, or the union is untested"
    );
}

/// The dangerous case, against a real database: a subject nothing names still has to be
/// distinguishable from a subject nothing could name.
///
/// The seeded store holds one alert, naming somebody else. Both calls return no rows.
///
/// PROVE THIS CAN FAIL: collapse `FindingsOutcome`'s `NotNamed`/`NotSearched` arms and the
/// inequality fails.
#[tokio::test]
async fn db_an_empty_section_still_says_which_kind_of_empty_it_is() {
    if !db_tests_enabled() {
        eprintln!("skipping: URBAN_DB_TESTS not set");
        return;
    }
    let db = TestNs::connect("findings_by_entity_empty_kinds").await;

    let rows = vec![motif_row(
        "x9995-elsewhere",
        "AddressCluster",
        0.40,
        &[OTHER],
    )];
    let seeded = rows.len();
    insert_rows(&db, rows).await;
    assert_eq!(count_events(&db).await, seeded, "seed bookkeeping");

    let cov = coverage();

    let searched = [finding(
        "names-nodes-probe",
        vec![row("names somebody else", vec![entity_ref(OTHER)])],
    )];
    let not_named = query_findings_naming_entity(
        &db,
        SUBJECT,
        Some(SUBJECT_CH),
        &searched,
        &cov,
        Some("2026-09-15T10:11:00Z"),
    )
    .await
    .expect("section read");

    let unsearchable = [finding(
        "aggregate-probe",
        vec![row("41,905 nodes", vec![])],
    )];
    let not_searched = query_findings_naming_entity(
        &db,
        SUBJECT,
        Some(SUBJECT_CH),
        &unsearchable,
        &cov,
        Some("2026-09-15T10:11:00Z"),
    )
    .await
    .expect("section read");

    assert!(not_named.rows.is_empty() && not_searched.rows.is_empty());
    assert_ne!(
        not_named.outcome, not_searched.outcome,
        "through the DB read path too: the two empties are different answers"
    );
    assert_eq!(not_named.outcome, FindingsOutcome::NotNamed);
    assert_eq!(not_searched.outcome, FindingsOutcome::NotSearched);
}
