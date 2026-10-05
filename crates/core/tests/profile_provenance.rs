//! PIV-03 / D-04: every profile row says where it came from, and a row that cannot is visibly
//! unattributed rather than quietly given a plausible source.
//!
//! # The failure this file exists to catch
//!
//! Every shortcut available here looks like success. A `source: "unknown"` string, a
//! `timestamp: 0` default, a `#[serde(default)]` on a provenance field, an `unwrap_or_default()`
//! on a missing `graph_events` timestamp — each produces a fully-populated profile that passes
//! every count-based test and lies to the analyst on every row it touched. **A placeholder
//! provenance is worse than none: it makes an unattributed fact look sourced.**
//!
//! So the load-bearing test in this file is the negative one
//! ([`db_a_row_whose_provenance_cannot_be_established_is_not_an_attributed_fact`]): a fixture
//! seeds a destination in a namespace no mapper in this workspace mints, and asserts the row comes
//! back `Unattributed` **with a reason**. A test that only checked "provenance is present where the
//! data supports it" would pass on a codebase that defaults everything.
//!
//! # The fact that shapes all of this
//!
//! `graph_events` records no source. `crates/core/src/pipeline/mod.rs`'s `to_mutation` receives an
//! `IngestEvent` carrying `source: EventSource` and returns a `GraphMutation` that has no such
//! field, so the adapter's identity is dropped before the log is written. Every dataset name for a
//! `graph_events`-derived fact is therefore an inference from the id namespace, and is made only
//! where exactly one producer mints that namespace. `GB-COH:`, `director:` and `location:` are each
//! minted by two or more, which is why the header, officer and address sections below are asserted
//! **unattributed** — that is the correct answer on this data, not a gap in the fixture.
//!
//! # Non-vacuity
//!
//! Sections are counted before they are classified, and the classification is asserted to contain
//! rows of **both** kinds across the profile. A run in which every section came back empty would
//! otherwise report "0 attributed, 0 unattributed, no violations" and read as a pass.
//!
//! # Running it
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test profile_provenance
//! ```
//!
//! Gated on `URBAN_DB_TESTS=1`, skipping by returning, so a gated run reports the same passing
//! count as an ungated one and duration is the only witness that a database was reached.
//! Namespaces come from `urban-integrity-test-support`, which drops them.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    detector_provenance, graph_event_provenance, identity_link_provenance, query_entity_by_id,
    relation_provenance, LinkTier, Provenance,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};

// ─────────────────────────────────────────────────────────────────────────────
// The four provenance shapes, without a database
// ─────────────────────────────────────────────────────────────────────────────

fn attributed(p: &Provenance) -> Option<(&str, &str, Option<LinkTier>)> {
    match p {
        Provenance::Attributed {
            source_dataset,
            observed_at,
            tier,
        } => Some((source_dataset, observed_at, *tier)),
        Provenance::Unattributed { .. } => None,
    }
}

fn reason(p: &Provenance) -> Option<&str> {
    match p {
        Provenance::Unattributed { reason } => Some(reason),
        Provenance::Attributed { .. } => None,
    }
}

/// A timestamp that exists, and the ISO rendering it must produce.
const TS_MS: u64 = 1_700_000_000_000;
const TS_ISO_PREFIX: &str = "2023-11-14T";

/// A namespace with exactly one minter is attributed; one with several is not, and says why.
///
/// PROVE THIS CAN FAIL: give `attribution::dataset_of_id`'s `map_or` fallback
/// `SourceAttribution::Dataset(COMPANIES_HOUSE)` instead of an `Ambiguous`, and the `GB-COH:`
/// half fails. That mutation was run before this test was committed.
#[test]
fn a_namespace_with_two_minters_is_never_given_one_of_them() {
    let cf = graph_event_provenance("Contract", "CF:ocds-b5fd17-0001", Some(TS_MS));
    let (dataset, observed_at, tier) = attributed(&cf).expect("a CF: contract must be attributed");
    assert_eq!(dataset, "CF");
    assert!(observed_at.starts_with(TS_ISO_PREFIX), "got {observed_at}");
    assert_eq!(tier, None, "a primary-dataset fact is not link-derived");

    for ambiguous in [
        "GB-COH:99000001",
        "director:jane-smith",
        "location:1-high-st",
    ] {
        let p = graph_event_provenance("Entity", ambiguous, Some(TS_MS));
        let why = reason(&p).unwrap_or_else(|| panic!("{ambiguous} must not be attributed"));
        assert!(
            why.len() > 20,
            "{ambiguous} must say WHY it cannot be attributed, got {why:?}"
        );
    }
}

/// An id in no known namespace is unattributed, not defaulted to the commonest source.
#[test]
fn an_unknown_namespace_does_not_fall_back_to_a_plausible_source() {
    let p = graph_event_provenance("Contract", "wat:x9997-unknown-00001", Some(TS_MS));
    assert!(
        reason(&p).is_some(),
        "an unrecognised namespace must be Unattributed, got {p:?}"
    );
}

/// A missing timestamp is unattributed, never the epoch.
///
/// `parse_alert_row` renders a missing timestamp as `1970-01-01T00:00:00Z` for its display field.
/// That is the exact placeholder a provenance chip must never carry: an analyst cannot tell a
/// genuinely ancient record from a lost one.
///
/// PROVE THIS CAN FAIL: replace `observed_at_from_ms`'s `let … else` with
/// `.unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())` and both halves fail. That mutation was
/// run before this test was committed.
#[test]
fn a_missing_timestamp_is_unattributed_not_the_epoch() {
    for ts in [None, Some(0)] {
        let p = graph_event_provenance("Contract", "CF:ocds-b5fd17-0001", ts);
        let why = reason(&p).unwrap_or_else(|| panic!("ts={ts:?} must not be attributed"));
        assert!(why.contains("timestamp"), "got {why:?}");
    }
    let d = detector_provenance(None);
    assert!(reason(&d).is_some(), "got {d:?}");
}

/// ENR-11 at the producer: a `pending_review` link cannot be rendered as an attributed fact, and
/// neither can any other status outside the two-token tier vocabulary.
///
/// This is the second gate, behind the `status IN ('auto_linked','confirmed')` filter the scans
/// apply. A new surface leaking a candidate link is a regression against a shipped rule.
///
/// PROVE THIS CAN FAIL: add a `_ => LinkTier::AutoLinked` arm to `identity_link_provenance`'s
/// match and the `pending_review` half fails. That mutation was run before this test was
/// committed.
#[test]
fn a_pending_review_link_is_never_an_attributed_fact() {
    let when = chrono::DateTime::parse_from_rfc3339("2024-04-12T00:00:00Z")
        .expect("fixture timestamp")
        .with_timezone(&chrono::Utc);

    for status in ["pending_review", "rejected", ""] {
        let p = identity_link_provenance("charity_commission", status, when);
        assert!(
            reason(&p).is_some(),
            "status {status:?} must not produce an attributed fact, got {p:?}"
        );
    }

    for (status, expected) in [
        ("confirmed", LinkTier::Confirmed),
        ("auto_linked", LinkTier::AutoLinked),
    ] {
        let p = identity_link_provenance("charity_commission", status, when);
        let (dataset, observed_at, tier) = attributed(&p).expect("a surfaced tier is attributed");
        assert_eq!(dataset, "CC");
        assert!(observed_at.starts_with("2024-04-12"), "got {observed_at}");
        assert_eq!(tier, Some(expected));
    }
}

/// An adapter with no glossary token is attributed under **its own name**, which is a real
/// attribution — never `"unknown"`, which is not. An empty source is the only unattributable case.
#[test]
fn an_unmapped_adapter_names_itself_rather_than_shrugging() {
    let when = chrono::DateTime::parse_from_rfc3339("2024-04-12T00:00:00Z")
        .expect("fixture timestamp")
        .with_timezone(&chrono::Utc);

    let p = identity_link_provenance("planning_identity", "auto_linked", when);
    let (dataset, _, _) = attributed(&p).expect("an unmapped adapter still names itself");
    assert_eq!(dataset, "planning_identity");

    let empty = identity_link_provenance("", "auto_linked", when);
    assert!(reason(&empty).is_some(), "got {empty:?}");
}

/// A relation row with no readable `meta.created_at` is unattributed rather than stamped "now".
///
/// PROVE THIS CAN FAIL: make `relation_provenance` fall back to
/// `chrono::Utc::now().to_rfc3339()` when `created_at_raw` is absent, and the second half fails.
/// That mutation was run before this test was committed.
#[test]
fn a_relation_row_with_no_created_at_is_unattributed_not_stamped_now() {
    let ts = serde_json::json!("2024-04-12T00:00:00Z");
    let ok = relation_provenance("trustee_of", "entity:`GB-COH:1`", Some(&ts));
    let (dataset, observed_at, _) = attributed(&ok).expect("trustee_of has one producer");
    assert_eq!(dataset, "CC");
    assert_eq!(observed_at, "2024-04-12T00:00:00Z");

    for absent in [None, Some(&serde_json::Value::Null)] {
        let p = relation_provenance("trustee_of", "entity:`GB-COH:1`", absent);
        assert!(reason(&p).is_some(), "got {p:?}");
    }
}

/// The detector token is an honest producer name, not a register's.
#[test]
fn a_detector_finding_is_not_dressed_up_as_a_register() {
    let p = detector_provenance(Some(TS_MS));
    let (dataset, observed_at, tier) = attributed(&p).expect("a dated finding is attributed");
    assert_eq!(dataset, "detector");
    assert!(observed_at.starts_with(TS_ISO_PREFIX));
    assert_eq!(tier, None);
}

// ─────────────────────────────────────────────────────────────────────────────
// The whole profile, against a database
// ─────────────────────────────────────────────────────────────────────────────

/// Synthetic `X9997…` numbering, never a live analysis id.
const CH: &str = "X9997001";
const SUBJECT: &str = "GB-COH:X9997001";
/// A second subject, in a namespace with exactly one minter, so the header section is observed
/// attributed as well as unattributed. Without it "the header is always unattributed" would be
/// consistent with a header that is never attributed at all.
const MUTUAL_SUBJECT: &str = "GB-MUT:X9997R";

const CF_CONTRACT: &str = "CF:x9997-cf-00001";
const PLANNING_APP: &str = "planning:x9997-app-00001";
/// The trap. No mapper in this workspace mints this namespace, so nothing can name its producer.
/// It routes to the contracts section (everything that is not `planning:` is a contract), which
/// puts an unattributable row **beside** attributable ones in the same section.
const UNKNOWN_NS_CONTRACT: &str = "wat:x9997-unknown-00001";
const LOCATION: &str = "location:1-x9997-street,-llangefni";
const OFFICER: &str = "director:x9997-jane-smith";
const PSC_ID: &str = "psc:x9997-psc-00001";
const CHARITY: &str = "x9997-charity-00001";
const PARTY: &str = "PA:x9997-party";
const PROPERTY: &str = "X9997001_TITLE1";

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

async fn seed_identity_link(
    db: &Surreal<Any>,
    source_node_id: &str,
    target_node_id: &str,
    source: &str,
    status: &str,
    evidence_json: &str,
) {
    db.query(
        "CREATE identity_links CONTENT { \
             source_node_id: $sni, target_node_id: $tni, \
             resolution_type: 'exact_company_number', confidence: 0.95, \
             source: $src, resolved_at: d'2024-04-12T00:00:00Z', \
             evidence_json: $ev, status: $st }",
    )
    .bind(("sni", source_node_id.to_string()))
    .bind(("tni", target_node_id.to_string()))
    .bind(("src", source.to_string()))
    .bind(("ev", evidence_json.to_string()))
    .bind(("st", status.to_string()))
    .await
    .expect("identity_links CREATE")
    .check()
    .expect("identity_links CREATE statement");
}

async fn seed(db: &Surreal<Any>) {
    let entity_weight = |name: &str, ch: serde_json::Value| {
        serde_json::json!({ "type": "Entity", "name": name, "company_number": ch,
                            "status": { "type": "Active" } })
    };
    let contract_weight = |name: &str, refs: serde_json::Value| {
        let mut w = serde_json::json!({ "type": "Contract", "name": name, "status": "awarded",
                                        "ward_code": "X05999001" });
        for (k, v) in refs.as_object().expect("refs object") {
            w[k] = v.clone();
        }
        w
    };

    let rows = vec![
        node_row(
            "x9997-subject",
            SUBJECT,
            entity_weight("X9997 Subject Ltd", serde_json::json!(CH)),
        ),
        node_row(
            "x9997-mutual",
            MUTUAL_SUBJECT,
            entity_weight("X9997 Mutual Society", serde_json::Value::Null),
        ),
        node_row(
            "x9997-cf-node",
            CF_CONTRACT,
            contract_weight(
                "X9997 CF Award",
                serde_json::json!({ "tender_ref": "X9997-TENDER-1", "value_gbp": 125000.0 }),
            ),
        ),
        edge_row(
            "x9997-cf-edge",
            SUBJECT,
            CF_CONTRACT,
            edge_weight("AwardedTo"),
        ),
        node_row(
            "x9997-app-node",
            PLANNING_APP,
            contract_weight(
                "X9997 Planning Application",
                serde_json::json!({ "application_ref": "X9997C001" }),
            ),
        ),
        edge_row(
            "x9997-app-edge",
            SUBJECT,
            PLANNING_APP,
            edge_weight("AwardedTo"),
        ),
        node_row(
            "x9997-unknown-node",
            UNKNOWN_NS_CONTRACT,
            contract_weight(
                "X9997 Contract From Nowhere",
                serde_json::json!({ "tender_ref": "X9997-TENDER-NOWHERE" }),
            ),
        ),
        edge_row(
            "x9997-unknown-edge",
            SUBJECT,
            UNKNOWN_NS_CONTRACT,
            edge_weight("AwardedTo"),
        ),
        node_row(
            "x9997-loc-node",
            LOCATION,
            serde_json::json!({ "type": "Location", "name": "1 X9997 Street",
                                "postcode": "LL77 7AA", "address_line_1": "1 X9997 Street",
                                "locality": "Llangefni" }),
        ),
        edge_row(
            "x9997-loc-edge",
            SUBJECT,
            LOCATION,
            edge_weight("LocatedAt"),
        ),
        node_row(
            "x9997-officer-node",
            OFFICER,
            serde_json::json!({ "type": "Officer", "name": "Jane Smith",
                                "officer_role": "director", "appointed_on": "2020-01-01" }),
        ),
        edge_row(
            "x9997-directs-edge",
            OFFICER,
            SUBJECT,
            edge_weight("Directs"),
        ),
        node_row(
            "x9997-psc-node",
            PSC_ID,
            serde_json::json!({ "type": "Psc", "name": "Jane Smith",
                                "natures_of_control": ["ownership-of-shares-75-to-100-percent"] }),
        ),
        node_row(
            "x9997-charity-node",
            CHARITY,
            serde_json::json!({ "type": "Charity", "name": "X9997 Helping Hands" }),
        ),
        node_row(
            "x9997-party-node",
            PARTY,
            serde_json::json!({ "type": "PoliticalActor", "name": "X9997 Party" }),
        ),
        // A MotifDetected alert naming the subject, so the alerts section is non-empty.
        serde_json::json!({
            "event_id": "x9997-alert",
            "timestamp": ts(),
            "kind": "MotifDetected",
            "payload": { "result_json": serde_json::json!({
                "motif_type": "AddressCluster",
                "risk_score": 0.91,
                "participants": [SUBJECT],
                "score_breakdown": { "ward_code": "X05999001" },
            }).to_string() },
        }),
    ];
    insert_rows(db, rows).await;

    let entity_thing = format!("entity:`{SUBJECT}`");
    let psc_thing = format!("psc:`{PSC_ID}`");
    for stmt in [
        format!(
            "RELATE {entity_thing}->owns_property->property:`{PROPERTY}` CONTENT {{ \
             meta: {{ created_at: d'2024-04-12T00:00:00Z', effective_from: NONE, \
             effective_to: NONE }}, data: {{ price_paid: 450000.0, tenure: 'Freehold' }} \
             }} RETURN NONE"
        ),
        format!(
            "RELATE {entity_thing}->trustee_of->charity:`{CHARITY}` CONTENT {{ \
             meta: {{ created_at: d'2024-04-12T00:00:00Z', effective_from: NONE, \
             effective_to: NONE }} }} RETURN NONE"
        ),
        format!(
            "RELATE {entity_thing}->donates_to->political_actor:`{PARTY}` CONTENT {{ \
             meta: {{ created_at: d'2024-04-12T00:00:00Z', effective_from: NONE, \
             effective_to: NONE }} }} RETURN NONE"
        ),
        format!("CREATE {psc_thing} CONTENT {{ name: 'Jane Smith', entity_id: '{PSC_ID}' }}"),
        format!(
            "RELATE {psc_thing}->controls->{entity_thing} CONTENT {{ \
             meta: {{ created_at: d'2024-04-12T00:00:00Z', effective_from: NONE, \
             effective_to: NONE }} }} RETURN NONE"
        ),
    ] {
        db.query(stmt)
            .await
            .expect("relation seed")
            .check()
            .expect("relation seed statement");
    }

    // Surfaced: an auto_linked Charity Commission link.
    seed_identity_link(
        db,
        SUBJECT,
        "charity:x9997-cc",
        "charity_commission",
        "auto_linked",
        "{}",
    )
    .await;
    // Surfaced: a confirmed OpenSanctions match, so `sanctions_status` is populated.
    seed_identity_link(
        db,
        SUBJECT,
        "sanction:x9997-os",
        "opensanctions",
        "confirmed",
        "{}",
    )
    .await;
    // NEVER surfaced (ENR-11). Its presence in the table is asserted by `count()` below, so
    // "it did not leak" is a statement about the filter rather than about an empty table.
    seed_identity_link(
        db,
        SUBJECT,
        "charity:x9997-candidate",
        "charity_commission",
        "pending_review",
        "{}",
    )
    .await;
}

async fn count_identity_links(db: &Surreal<Any>, status: &str) -> usize {
    let n: Option<i64> = db
        .query("SELECT count() FROM identity_links WHERE status = $st GROUP ALL")
        .bind(("st", status.to_string()))
        .await
        .expect("count query")
        .take((0, "count"))
        .expect("count take");
    n.unwrap_or(0) as usize
}

/// Every provenance on the profile, labelled by the section it came from.
fn every_provenance(
    profile: &urban_integrity_core::EntityProfile,
) -> Vec<(&'static str, Provenance)> {
    let mut out: Vec<(&'static str, Provenance)> =
        vec![("identifiers", profile.identity_provenance.clone())];
    let push =
        |out: &mut Vec<(&'static str, Provenance)>, section: &'static str, ps: Vec<Provenance>| {
            for p in ps {
                out.push((section, p));
            }
        };
    push(
        &mut out,
        "addresses",
        profile
            .addresses
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "officers",
        profile
            .directors
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "contracts",
        profile
            .contracts
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "planning_applications",
        profile
            .planning_applications
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "owns_property",
        profile
            .owns_property
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "trustee_of",
        profile
            .trustee_of
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "donates_to",
        profile
            .donates_to
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "offshore_links",
        profile
            .offshore_links
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "osint_links",
        profile
            .osint_links
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "beneficial_owners",
        profile
            .beneficial_owners
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "alerts",
        profile
            .alerts
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "osint_alerts",
        profile
            .osint_alerts
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    push(
        &mut out,
        "related_entities",
        profile
            .related_entities
            .iter()
            .map(|r| r.provenance.clone())
            .collect(),
    );
    if let Some(s) = &profile.sanctions_status {
        out.push(("sanctions_status", s.provenance.clone()));
    }
    out
}

/// **The negative test.** A row whose provenance cannot be established does not render as an
/// attributed fact — it is `Unattributed`, carrying a reason, sitting beside attributed rows in
/// the same section.
///
/// PROVE THIS CAN FAIL: give `attribution::dataset_of_id` any `Dataset(...)` fallback for an
/// unrecognised namespace and this fails on the trap row. That mutation was run before this test
/// was committed.
#[tokio::test]
async fn db_a_row_whose_provenance_cannot_be_established_is_not_an_attributed_fact() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_a_row_whose_provenance_cannot_be_established_is_not_an_attributed_fact \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_provenance_negative").await;
    seed(&db).await;

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("profile query")
        .expect("the seeded subject must resolve");

    // ── Non-vacuity: the section holds BOTH kinds of row ─────────────────────
    assert_eq!(
        profile.contracts.len(),
        2,
        "the contracts section must hold the CF award AND the unattributable trap row, so the \
         assertions below are comparing two populations rather than one; got {:?}",
        profile
            .contracts
            .iter()
            .map(|c| &c.name)
            .collect::<Vec<_>>()
    );

    let trap = profile
        .contracts
        .iter()
        .find(|c| c.name == "X9997 Contract From Nowhere")
        .expect("the trap contract must be in the contracts section");
    let why = reason(&trap.provenance).unwrap_or_else(|| {
        panic!(
            "a destination in a namespace no mapper mints must NOT be attributed; got {:?}",
            trap.provenance
        )
    });
    assert!(
        !why.trim().is_empty(),
        "an Unattributed row must say why, or it is a blank wearing a type"
    );

    let real = profile
        .contracts
        .iter()
        .find(|c| c.name == "X9997 CF Award")
        .expect("the CF award must be in the contracts section");
    let (dataset, observed_at, _) = attributed(&real.provenance).unwrap_or_else(|| {
        panic!(
            "a CF: destination must be attributed; got {:?}",
            real.provenance
        )
    });
    assert_eq!(dataset, "CF");
    assert!(observed_at.starts_with(TS_ISO_PREFIX), "got {observed_at}");
}

/// Every row on the profile carries a provenance that is either a real attribution or an honest
/// refusal — never an empty source, never the epoch, never a blank reason.
///
/// PROVE THIS CAN FAIL: default `Provenance` on any row type to
/// `Attributed { source_dataset: String::new(), observed_at: "1970-01-01T00:00:00Z", tier: None }`
/// and this fails on that section by name. That mutation was run before this test was committed.
#[tokio::test]
async fn db_no_row_on_the_profile_carries_a_placeholder_provenance() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_no_row_on_the_profile_carries_a_placeholder_provenance (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_provenance_placeholder").await;
    seed(&db).await;

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("profile query")
        .expect("the seeded subject must resolve");

    let all = every_provenance(&profile);

    // ── Non-vacuity, before anything is classified ───────────────────────────
    assert!(
        all.len() >= 12,
        "the fixture must populate most sections or this proves nothing; got {} rows across {:?}",
        all.len(),
        all.iter()
            .map(|(s, _)| *s)
            .collect::<std::collections::BTreeSet<_>>()
    );
    let n_attributed = all.iter().filter(|(_, p)| attributed(p).is_some()).count();
    let n_unattributed = all.len() - n_attributed;
    assert!(
        n_attributed > 0 && n_unattributed > 0,
        "the fixture must produce BOTH kinds — {n_attributed} attributed, {n_unattributed} \
         unattributed — or this test cannot tell a correct classifier from a constant one"
    );

    for (section, p) in &all {
        match p {
            Provenance::Attributed {
                source_dataset,
                observed_at,
                ..
            } => {
                assert!(
                    !source_dataset.trim().is_empty(),
                    "{section}: an attributed row must name a dataset"
                );
                assert!(
                    !source_dataset.eq_ignore_ascii_case("unknown"),
                    "{section}: \"unknown\" is a placeholder wearing an attribution"
                );
                let parsed =
                    chrono::DateTime::parse_from_rfc3339(observed_at).unwrap_or_else(|e| {
                        panic!("{section}: observed_at {observed_at:?} is not RFC-3339: {e}")
                    });
                assert!(
                    parsed.timestamp() > 0,
                    "{section}: observed_at {observed_at:?} is the epoch — a lost timestamp \
                     rendered as an ancient one"
                );
            }
            Provenance::Unattributed { reason } => {
                assert!(
                    !reason.trim().is_empty(),
                    "{section}: an unattributed row must carry the reason"
                );
            }
        }
    }
}

/// The per-section classification this corpus actually produces, pinned so a later change that
/// quietly starts attributing an ambiguous namespace has to say so here.
///
/// PROVE THIS CAN FAIL: remove `"Directs"` from `attribution::UNATTRIBUTABLE_TERMS` and the
/// officers row fails — `director:` would then resolve through `dataset_of_id`, which is still
/// ambiguous, so the message names the term rather than the namespace. That mutation was run
/// before this test was committed.
#[tokio::test]
async fn db_each_section_is_attributed_or_refused_for_the_documented_reason() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_each_section_is_attributed_or_refused_for_the_documented_reason \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_provenance_sections").await;
    seed(&db).await;

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("profile query")
        .expect("the seeded subject must resolve");

    // Attributed sections, with the dataset each must name.
    assert_eq!(
        attributed(&profile.planning_applications[0].provenance).map(|(d, _, _)| d),
        Some("LPA"),
        "a planning: destination is the Local Planning Authority's"
    );
    assert_eq!(
        attributed(&profile.owns_property[0].provenance).map(|(d, _, _)| d),
        Some("CCOD"),
        "an entity:-sided owns_property row is the CCOD extract"
    );
    assert_eq!(
        attributed(&profile.trustee_of[0].provenance).map(|(d, _, _)| d),
        Some("CC"),
    );
    assert_eq!(
        attributed(&profile.donates_to[0].provenance).map(|(d, _, _)| d),
        Some("EC"),
    );
    assert_eq!(
        attributed(&profile.beneficial_owners[0].provenance).map(|(d, _, _)| d),
        Some("CH"),
        "a controls edge has one producer, ch_psc"
    );
    assert_eq!(
        attributed(&profile.alerts[0].provenance).map(|(d, _, _)| d),
        Some("detector"),
        "a motif alert is this project's assertion, not a register's"
    );

    // Refused sections, each for a reason the registry records.
    assert!(
        !profile.addresses.is_empty(),
        "NON-VACUITY: the address section came back empty, so asserting it is unattributed \
         would prove nothing"
    );
    let addr_reason = reason(&profile.addresses[0].provenance)
        .unwrap_or_else(|| panic!("got {:?}", profile.addresses[0].provenance));
    assert!(
        addr_reason.contains("location:"),
        "the address refusal must name the namespace it is about, got {addr_reason:?}"
    );

    assert!(
        !profile.directors.is_empty(),
        "NON-VACUITY: the officers section came back empty"
    );
    let officer_reason = reason(&profile.directors[0].provenance)
        .unwrap_or_else(|| panic!("got {:?}", profile.directors[0].provenance));
    assert!(
        officer_reason.contains("Directs"),
        "the officer refusal must name the term with two producers, got {officer_reason:?}"
    );

    let header_reason = reason(&profile.identity_provenance)
        .unwrap_or_else(|| panic!("got {:?}", profile.identity_provenance));
    assert!(
        header_reason.contains("GB-COH"),
        "the header refusal must name the namespace, got {header_reason:?}"
    );

    // ...and the header IS attributed where the namespace has one minter, so "always refused" is
    // not what is being pinned.
    let mutual = query_entity_by_id(&db, MUTUAL_SUBJECT)
        .await
        .expect("profile query")
        .expect("the seeded mutual society must resolve");
    assert_eq!(
        attributed(&mutual.identity_provenance).map(|(d, _, _)| d),
        Some("FCA"),
        "GB-MUT: is minted only by the FCA Mutuals adapter"
    );
}

/// ENR-11 on the new surface: a `pending_review` link is in the table and is not on the profile,
/// and every link that IS on the profile carries a tier.
///
/// PROVE THIS CAN FAIL: add `"pending_review"` to the statuses `assemble_entity_profile` scans for
/// and the leak assertion fails naming the target id. That mutation was run before this test was
/// committed.
#[tokio::test]
async fn db_a_candidate_link_is_in_the_table_and_not_on_the_profile() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: db_a_candidate_link_is_in_the_table_and_not_on_the_profile \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = TestNs::connect("profile_provenance_enr11").await;
    seed(&db).await;

    // ── Non-vacuity: the candidate link exists, measured independently ───────
    // Without this the "no leak" assertion below would pass against an empty table.
    assert_eq!(
        count_identity_links(&db, "pending_review").await,
        1,
        "the fixture must actually contain a candidate link, or 'it did not leak' is vacuous"
    );
    assert_eq!(
        count_identity_links(&db, "auto_linked").await
            + count_identity_links(&db, "confirmed").await,
        2,
        "and it must contain surfaceable links too, or the filter could be rejecting everything"
    );

    let profile = query_entity_by_id(&db, SUBJECT)
        .await
        .expect("profile query")
        .expect("the seeded subject must resolve");

    assert!(
        !profile.osint_links.is_empty(),
        "NON-VACUITY: no OSINT link surfaced at all, so 'the candidate did not surface' proves \
         nothing about the filter"
    );
    assert!(
        !profile
            .osint_links
            .iter()
            .any(|l| l.target_node_id.contains("candidate")),
        "ENR-11: a pending_review link reached the profile — {:?}",
        profile
            .osint_links
            .iter()
            .map(|l| &l.target_node_id)
            .collect::<Vec<_>>()
    );

    for link in &profile.osint_links {
        let (_, _, tier) = attributed(&link.provenance).unwrap_or_else(|| {
            panic!(
                "a surfaced identity link records its own provenance; got {:?}",
                link.provenance
            )
        });
        assert!(
            tier.is_some(),
            "a link-derived fact must carry its confidence tier (PIV-03)"
        );
        assert_ne!(
            link.status, "pending_review",
            "the legacy status field must agree with the tier vocabulary"
        );
    }

    let sanctions = profile
        .sanctions_status
        .as_ref()
        .expect("the confirmed OpenSanctions link must populate sanctions_status");
    assert_eq!(
        attributed(&sanctions.provenance).map(|(d, _, t)| (d, t)),
        Some(("OpenSanctions", Some(LinkTier::Confirmed))),
    );
}
