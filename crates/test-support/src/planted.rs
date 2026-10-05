//! The planted fixture (spec §3): a ~82-node graph whose algorithm answers are known by
//! construction, plus a `Manifest` of those answers as literals. Three consumers share it:
//! `crates/workbench/tests/planted_golden.rs`, the `fixture seed --planted` CLI subcommand, and
//! the `workbench-dto` worked-example drift test. Every expectation in `Manifest` is a literal —
//! never computed from the graph at test time.

use std::collections::BTreeSet;

use chrono::{DateTime, TimeZone, Utc};
use urban_integrity_schema::types::edge::{
    AwardedToData, DeclaresInterestData, DirectsData, DisqualifiedFromData, EdgeMeta, EdgeWeight,
    IssuedByData, LocatedAtData, RepresentsData,
};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{
    ApplicantProvenance, CharityData, ContractData, DisqualificationData, EntityData, EntityStatus,
    LocationData, NodeWeight, OfficerData, OffshoreEntityData, PoliticalActorData, PropertyData,
    PscData, WardData,
};

/// One planted edge. `src`/`dst` are the same ids the `graph_events` payload carries.
#[derive(Debug, Clone, PartialEq)]
pub struct PlantedEdge {
    pub src: EntityId,
    pub dst: EntityId,
    pub weight: EdgeWeight,
}

/// An expected path: the exact node ids in order, first to last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPath {
    pub from: &'static str,
    pub to: &'static str,
    pub edge_types_included: &'static [&'static str],
    pub node_ids: Vec<&'static str>,
}

/// An expected *absence* of a path: under `edge_types_included`, `from` cannot reach `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedNoPath {
    pub from: &'static str,
    pub to: &'static str,
    pub edge_types_included: &'static [&'static str],
}

/// An expected all-simple-paths count under a hop cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPathSet {
    pub from: &'static str,
    pub to: &'static str,
    pub edge_types_included: &'static [&'static str],
    pub hop_cap: u32,
    pub count: usize,
}

/// An expected Louvain partition: `sets` are the communities that must exist (each exactly);
/// `flexible` ids may sit in any set, and are removed from both sides before comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCommunitySet {
    pub edge_types_included: &'static [&'static str],
    pub seed: u64,
    pub min_size: u32,
    pub sets: Vec<BTreeSet<&'static str>>,
    pub flexible: Vec<&'static str>,
    pub hidden_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedKCore {
    pub k: u32,
    pub edge_types_included: &'static [&'static str],
    pub members: BTreeSet<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedBetweenness {
    pub edge_types_included: &'static [&'static str],
    pub top_1: &'static str,
    /// Exact score `sampled_betweenness` returns with every node as a pivot — twice the textbook
    /// undirected value because the implementation does not halve.
    pub top_1_score: f64,
}

/// The side-card check sample: one node's degree and temporal envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideCardSample {
    pub id: &'static str,
    pub degree: u32,
    /// Earliest `effective_from` across the node's edges, `YYYY-MM-DD`.
    pub earliest_from: &'static str,
    /// Latest `effective_to`, `YYYY-MM-DD`, or `None` when every edge is open-ended.
    pub latest_to: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedWardSubgraph {
    pub ward_id: &'static str,
    pub members: BTreeSet<&'static str>,
}

// ── Profile expectations (plan 45-16) ────────────────────────────────────────
//
// Added for the phase-45 verification gate, whose rule is that every checklist item carries a
// **toy block above its live block**: the operator reads the expected figure here, sees it on a
// fixture whose answer is known by construction, and only then looks at a corpus where nothing
// can say whether what is on screen is right.
//
// These are as literal as the algorithm expectations above, and for the same reason. What they
// are literal *about* is different: an algorithm answer is a property of the topology, while a
// profile answer is a property of the topology **and** of the read path's direction conventions
// — `Directs` is counted where `dst` is the subject, `AwardedTo` where `src` is. Getting that
// backwards is not hypothetical: it is the live defect that leaves Officers empty on 99.8% of
// this project's corpus. Writing the numbers out by hand from `topology()` and then checking
// them through the real HTTP route is what makes the two independent.

/// One section of a profile and the number of rows it holds, named by its `EntityProfileDto`
/// field so a reader can go from the checklist to the wire without a mapping table.
pub type SectionCount = (&'static str, usize);

/// One subject's whole profile, as the fixture is built to render it.
///
/// **Every section is listed, including the zeros.** An expectation that named only the non-empty
/// sections would be silent about exactly the question this fixture exists to settle — an analyst
/// looking at an empty Officers block needs to know whether it is empty because the entity has no
/// officers, because a filter hid them, or because the read path missed them, and only a manifest
/// that says "this one is zero on purpose" can tell the first from the third.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedProfile {
    pub id: &'static str,
    pub name: &'static str,
    /// `EntityStatusView`'s variant name: `"Active"`, `"Dissolved"` or `"InLiquidation"` (the
    /// last added by Phase 49's Structure 7 `NodeUpdated` flip on `B`).
    pub status: &'static str,
    /// Every `EntityProfileDto` paged section, with its `total`.
    pub sections: &'static [SectionCount],
    /// The ids the Related section names — the recursive-pivot surface (PIV-03). Every one of
    /// these opens a profile, because every one is an `Entity`; see `profile/mod.rs`'s 404 policy
    /// for why `director:`, `location:` and `contract:` ids do not.
    pub related_ids: &'static [&'static str],
}

/// One section's as-at tally under [`ExpectedAsAtProfile`]: how many of its rows the filter
/// placed in force, how many it placed outside, and how many it could not test.
///
/// Three numbers rather than one, because `AsAtStatus` has three answers a row can carry and
/// collapsing any two of them loses the distinction PIV-04 exists to draw. The three must sum to
/// the section's total in [`ExpectedProfile`] — nothing is removed by a filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionTally {
    pub section: &'static str,
    pub in_force: usize,
    pub out_of_force: usize,
    pub undated: usize,
}

/// One subject read at one date.
///
/// The fixture is built so that a single subject shows all three answers as the date moves, which
/// no subject on the live corpus can do: measured 2026-09-15, `effective_to` is null on every edge
/// in the store and `effective_from` on all but two edge types, so **every** Officers and
/// Addresses row there comes back `Undated`. The toy is therefore the only place the in-force and
/// outside chips can be seen at all, which is why the checklist runs it first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedAsAtProfile {
    pub id: &'static str,
    /// ISO-8601, the value that goes in `?as_at=`.
    pub as_at: &'static str,
    pub sections: &'static [SectionTally],
}

/// One disambiguation query and the list it returns (PIV-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedDisambiguation {
    pub query: &'static str,
    pub rows: usize,
    /// `DisambiguationView::more_may_exist` — true only when the read path's ten-match cap bit.
    pub more_may_exist: bool,
}

/// One neighbourhood expansion and the node count it reaches (CNV-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedNeighbourhood {
    pub id: &'static str,
    pub hops: u32,
    /// Neighbours reached, excluding the subject itself.
    pub total: usize,
    /// The ids reached, so a cap can be checked against a known set rather than a known size.
    pub node_ids: &'static [&'static str],
}

/// Every answer the fixture is built to give. All literals.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub seeded_nodes: usize,
    pub seeded_edges: usize,
    pub reachable_nodes: usize,
    pub reachable_edges: usize,
    pub ward_code: &'static str,
    /// The local planning authority code. Unlike `ward_code` this names **no node** in the
    /// fixture — it exists only to fill the toy analysis registry row the seeder writes
    /// alongside the graph, so a consumer can register the planted analysis like a real one.
    pub lpa_code: &'static str,
    pub shortest_paths: Vec<ExpectedPath>,
    pub no_path: Vec<ExpectedNoPath>,
    pub all_simple_paths: Vec<ExpectedPathSet>,
    pub communities: Vec<ExpectedCommunitySet>,
    pub k_cores: Vec<ExpectedKCore>,
    pub betweenness: Vec<ExpectedBetweenness>,
    pub ward_subgraphs: Vec<ExpectedWardSubgraph>,
    /// The side card check sample: the bridge officer.
    pub side_card_sample: SideCardSample,
    /// Plan 45-16: the entity-profile screen's expectations, for the QA gate's toy block.
    pub profiles: Vec<ExpectedProfile>,
    /// Plan 45-16: the same subject at three dates (PIV-04).
    pub as_at_profiles: Vec<ExpectedAsAtProfile>,
    /// Plan 45-16: the disambiguation list (PIV-01).
    pub disambiguations: Vec<ExpectedDisambiguation>,
    /// Plan 45-16: one neighbourhood expansion (CNV-01), small enough that a node cap set below
    /// `total` has an arithmetically checkable answer.
    pub neighbourhoods: Vec<ExpectedNeighbourhood>,
    /// Phase 49 (EVM-05, plan 49-11): each of the seven D-02-priority probes' expected total
    /// `RecordRef` count against this fixture, as a literal — see [`ExpectedProbeRefs`].
    pub evidence_refs: Vec<ExpectedProbeRefs>,
}

/// One priority probe's expected total `RecordRef` count against the planted fixture (Phase 49,
/// EVM-05, plan 49-11) — a literal, never computed at test time. `ref_count` is the SUM of
/// `finding.evidence.iter().map(|e| e.refs.len())` across the whole `Finding` a probe's `run()`
/// returns against the seeded fixture, cross-checked against each of 49-03..49-09's SUMMARY
/// before being written here (a plan re-deriving the fixture shape must re-verify these, not
/// trust them blindly — see [`Manifest::evidence_refs`]'s own doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedProbeRefs {
    pub probe_name: &'static str,
    pub ref_count: usize,
}

/// One planted `identity_links` row (Phase 49, EVM-04/EVM-05) — a seed input, not an algorithm
/// answer, so it lives on [`PlantedGraph`] rather than [`Manifest`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlantedIdentityLink {
    pub key: &'static str,
    pub source_node_id: &'static str,
    pub target_node_id: &'static str,
    pub confidence: f64,
    pub source: &'static str,
    pub status: &'static str,
}

/// One planted `MotifDetected` `graph_events` row (Phase 49, EVM-04/EVM-05).
#[derive(Debug, Clone, PartialEq)]
pub struct PlantedMotifEvent {
    pub event_id: &'static str,
    pub motif_type: &'static str,
    pub participants: &'static [&'static str],
    pub risk_score: f64,
}

/// One planted `land_flip_gate1_survivors` row (Phase 50 plan 10) — a side-table entry
/// `land_flip_runs.rs` reads directly, never through `graph_events`; `applicant_id`/`location_id`
/// are plain strings and do not need to be real graph nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct PlantedLandFlipSurvivor {
    pub planning_ref: &'static str,
    pub applicant_id: &'static str,
    pub location_id: &'static str,
}

/// The planted `land_flip_runs:current` marker plus its survivor rows (Phase 50 plan 10) — feeds
/// `land-flip-candidates`'s marker-first, population/capability-split verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct PlantedLandFlipRun {
    pub applications_scanned: i64,
    pub gate1_no_ppd_match: i64,
    pub candidate_apps: i64,
    pub survivors: Vec<PlantedLandFlipSurvivor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlantedGraph {
    pub nodes: Vec<(EntityId, NodeWeight)>,
    pub edges: Vec<PlantedEdge>,
    pub manifest: Manifest,
    /// Phase 49 (EVM-04/EVM-05): planted `identity_links` rows, seeded alongside the graph by
    /// `events::seed_planted`.
    pub identity_links: Vec<PlantedIdentityLink>,
    /// Phase 49: adapter names whose `enrichment_runs` marker this fixture seeds — proves the
    /// Clear-vs-DataGap branch for each.
    pub enrichment_run_sources: Vec<&'static str>,
    /// Phase 49: planted `MotifDetected` `graph_events` rows.
    pub motif_events: Vec<PlantedMotifEvent>,
    /// Phase 50 plan 10: the planted `land_flip_runs:current` marker + survivor rows.
    pub land_flip_run: PlantedLandFlipRun,
}

/// The fixture's ward and LPA codes. Both satisfy `naming::validate_ward_code`'s shape rule
/// (one uppercase letter + eight digits) while being impossible to mistake for a real place:
/// `X` is not an ONS nation+category prefix at all, and the `999` sequence is outside every
/// real range. The prefix matters as much as the sequence — `scripts/check-no-analysis-literals.sh`
/// rejects any `W05`-prefixed code in the workbench crates, and the arrow fixture built from this
/// graph is compiled into `crates/workbench-web`.
pub const WARD_CODE: &str = "X05999001";
pub const LPA_CODE: &str = "X06999001";
pub const NOISE_PAIRS: usize = 20;

/// D-01's default-checked edge types, mirrored here so the manifest can name them without a
/// dependency on `workbench-web`.
pub const DEFAULT_EDGES: &[&str] = &[
    "Directs",
    "AwardedTo",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
];
pub const DEFAULT_PLUS_LOCATED_AT: &[&str] = &[
    "Directs",
    "AwardedTo",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
    "LocatedAt",
];
pub const DEFAULT_PLUS_ISSUED_BY: &[&str] = &[
    "Directs",
    "AwardedTo",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
    "IssuedBy",
];
pub const DEFAULT_PLUS_REPRESENTS: &[&str] = &[
    "Directs",
    "AwardedTo",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
    "Represents",
];

// ── ids ──────────────────────────────────────────────────────────────────────────────────────
/// Structure 1: the officer who directs both the dissolved predecessor and its successor.
pub const O1: &str = "director:planted-o1";
/// Structure 1: the dissolved predecessor company.
pub const A: &str = "GB-COH:PL000001";
/// Structure 1: the successor company, incorporated after `A` dissolved.
pub const B: &str = "GB-COH:PL000002";
/// Structure 1: the shared registered address of `A` and `B`.
pub const L1: &str = "location:planted-l1";
/// Structure 2: the address the eight cluster companies share.
pub const L2: &str = "location:planted-l2";
/// Structure 4: the contract awarded to the phoenix successor `B`.
pub const C1: &str = "contract:planted-c1";
/// Structure 4: the contract awarded to block one's `Q[0]`.
pub const C2: &str = "contract:planted-c2";
/// Structure 4: the contract awarded to block two's `S[0]`.
pub const C3: &str = "contract:planted-c3";
/// Structure 4: the contract awarded to the first address-cluster company.
pub const C4: &str = "contract:planted-c4";
/// Structure 4: the buyer that issued all four contracts.
pub const BUY: &str = "GB-COH:PL000099";
/// Structure 3: the bridge officer joining the two K5,5 blocks, 2 edges per side.
pub const OB: &str = "director:planted-ob";
/// Structure 5: the disqualified officer, attached to block one via `Q[1]`.
pub const OD: &str = "director:planted-od";
/// Structure 5: `OD`'s disqualification node.
pub const DQ1: &str = "disq:planted-od-case";
/// Structure 4: the ward every contract is represented by; the root of the reachable set.
pub const WARD: &str = "ward:X05999001";
/// Structure 7 (Phase 49): the planted beneficial owner, controlling `A` via a `Controls` edge.
pub const PSC1: &str = "psc:planted-psc1";
/// Structure 7 (Phase 49): an isolated OCOD-producer property node (`country_incorporated` is
/// `Some`), no edges — deliberately unreachable from `WARD`, unlike `PSC1`.
pub const PROPERTY1: &str = "property:planted-prop1";
/// Structure 7 (Phase 49): an isolated offshore entity node, no edges.
pub const OFFSHORE1: &str = "offshore:planted-off1";
/// Structure 7 (Phase 49): an isolated charity node, no edges.
pub const CHARITY1: &str = "charity:planted-charity1";
/// Structure 8 (Phase 50 plan 10): a planning Contract with `applicant_provenance` set, feeding
/// `identity-integrity`'s `"planning:"`-prefixed `total==0` check. No edges — deliberately
/// isolated, like `PROPERTY1`.
pub const PLANNING_C1: &str = "planning:planted-pc1";
/// Structure 8 (Phase 50 plan 10): a council-shaped `PoliticalActor` (`council_name` set),
/// feeding `detector-readiness`'s Nepotism group and `phase20-council-registers`'s `pa` count.
pub const COUNCIL_PA1: &str = "political_actor:planted-pa1";
/// Structure 8 (Phase 50 plan 10): the plain company `COUNCIL_PA1` declares an interest in —
/// feeds `phase20-council-registers`'s `declares` count and `detector-readiness`'s
/// political-actor-edges term. Named to avoid the "Cluster"/"Block One"/"Alpha" substrings so
/// existing disambiguation-query counts stay untouched.
pub const COUNCIL_INTEREST_CO: &str = "GB-COH:PL000601";

/// Structure 3, block one: the five officers `p1..p5`, each directing every company in `Q`.
pub const P: [&str; 5] = [
    "director:planted-p1",
    "director:planted-p2",
    "director:planted-p3",
    "director:planted-p4",
    "director:planted-p5",
];
/// Structure 3, block one: the five companies `q1..q5`, each directed by every officer in `P`.
pub const Q: [&str; 5] = [
    "GB-COH:PL000301",
    "GB-COH:PL000302",
    "GB-COH:PL000303",
    "GB-COH:PL000304",
    "GB-COH:PL000305",
];
/// Structure 3, block two: the five officers `r1..r5`, each directing every company in `S`.
pub const R: [&str; 5] = [
    "director:planted-r1",
    "director:planted-r2",
    "director:planted-r3",
    "director:planted-r4",
    "director:planted-r5",
];
/// Structure 3, block two: the five companies `s1..s5`, each directed by every officer in `R`.
pub const S: [&str; 5] = [
    "GB-COH:PL000401",
    "GB-COH:PL000402",
    "GB-COH:PL000403",
    "GB-COH:PL000404",
    "GB-COH:PL000405",
];
/// Structure 2: the eight companies sharing address `L2`.
pub const L2_COMPANIES: [&str; 8] = [
    "GB-COH:PL000201",
    "GB-COH:PL000202",
    "GB-COH:PL000203",
    "GB-COH:PL000204",
    "GB-COH:PL000205",
    "GB-COH:PL000206",
    "GB-COH:PL000207",
    "GB-COH:PL000208",
];

fn l2_company(i: usize) -> &'static str {
    L2_COMPANIES[i - 1]
}
fn p(i: usize) -> &'static str {
    P[i - 1]
}
fn q(i: usize) -> &'static str {
    Q[i - 1]
}
fn r(i: usize) -> &'static str {
    R[i - 1]
}
fn s(i: usize) -> &'static str {
    S[i - 1]
}
// Structure 6's noise ids never appear in the manifest, so they stay `format!`-generated.
fn noise_officer(i: usize) -> String {
    format!("director:planted-n{i}")
}
fn noise_company(i: usize) -> String {
    format!("GB-COH:PL0009{i:02}")
}

/// A UTC instant at midnight, for edge timestamps.
fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
}

fn meta(from: DateTime<Utc>, to: Option<DateTime<Utc>>) -> EdgeMeta {
    EdgeMeta {
        created_at: at(2026, 1, 1),
        effective_from: Some(from),
        effective_to: to,
    }
}

fn company(
    id: &str,
    name: &str,
    status: EntityStatus,
    incorporated: chrono::NaiveDate,
) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: Some(id.trim_start_matches("GB-COH:").to_string()),
            status,
            incorporation_date: Some(incorporated),
        }),
    )
}

/// A plain calendar day, for incorporation dates.
fn day(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn officer(id: &str, name: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Officer(OfficerData {
            name: name.to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: None,
        }),
    )
}

fn location(id: &str, name: &str, postcode: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Location(LocationData {
            name: name.to_string(),
            postcode: Some(postcode.to_string()),
            address_line_1: Some(name.to_string()),
            locality: Some("Plantedtown".to_string()),
        }),
    )
}

fn contract(id: &str, name: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Contract(ContractData {
            name: name.to_string(),
            ocid: None,
            value_gbp: Some(50_000.0),
            status: Some("complete".to_string()),
            date_published: Some("2026-02-01".to_string()),
            ward_code: Some(WARD_CODE.to_string()),
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        }),
    )
}

/// Structure 8 (Phase 50 plan 10): a planning Contract, mirroring `contract()` but with
/// `ward_code`/`tender_ref` unset and `applicant_provenance` set explicitly — feeds
/// `identity-integrity`'s `"planning:"`-prefixed, `"type":"Contract"` filter.
fn planning_contract(
    id: &str,
    name: &str,
    provenance: Option<ApplicantProvenance>,
) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Contract(ContractData {
            name: name.to_string(),
            ocid: None,
            value_gbp: Some(50_000.0),
            status: Some("complete".to_string()),
            date_published: Some("2026-02-01".to_string()),
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: provenance,
            portal_detail_handle: None,
        }),
    )
}

/// Structure 8 (Phase 50 plan 10): a council-shaped `PoliticalActor`, feeding
/// `detector-readiness`'s Nepotism group and `phase20-council-registers`'s `pa` count.
fn political_actor(id: &str, name: &str, council_name: Option<&str>) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::PoliticalActor(PoliticalActorData {
            name: name.to_string(),
            party: None,
            council_name: council_name.map(str::to_string),
        }),
    )
}

/// Structure 8 (Phase 50 plan 10): a councillor's declared interest in a company, mirroring
/// `council_registers_political_actors.rs`'s own `seed_declares_interest` test helper.
fn declares_interest(src: &str, dst: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(src.to_string()),
        dst: EntityId(dst.to_string()),
        weight: EdgeWeight::DeclaresInterest {
            meta: meta(at(2022, 1, 1), None),
            data: DeclaresInterestData {
                council: Some("Planted Council".to_string()),
                category: None,
            },
        },
    }
}

/// Real direction (`crates/ingestor/src/ch/mapper.rs:184`): src = officer, dst = company.
fn directs(src: &str, dst: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(src.to_string()),
        dst: EntityId(dst.to_string()),
        weight: EdgeWeight::Directs {
            meta: meta(at(2021, 1, 1), None),
            data: DirectsData {
                details: "planted".to_string(),
                officer_role: Some("director".to_string()),
            },
        },
    }
}

/// Real direction (`crates/ingestor/src/ch/mapper.rs:108`): src = entity, dst = location.
fn located_at(src: &str, dst: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(src.to_string()),
        dst: EntityId(dst.to_string()),
        weight: EdgeWeight::LocatedAt {
            meta: meta(at(2021, 1, 1), None),
            data: LocatedAtData {
                details: "planted".to_string(),
            },
        },
    }
}

/// Real direction (`shared_exact_address.rs`): src = applicant entity, dst = contract.
fn awarded_to(applicant: &str, contract: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(applicant.to_string()),
        dst: EntityId(contract.to_string()),
        weight: EdgeWeight::AwardedTo {
            meta: meta(at(2026, 2, 1), None),
            data: AwardedToData {
                details: "planted".to_string(),
            },
        },
    }
}

/// Real direction (`cf/mapper.rs`): src = buyer, dst = contract.
fn issued_by(buyer: &str, contract: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(buyer.to_string()),
        dst: EntityId(contract.to_string()),
        weight: EdgeWeight::IssuedBy {
            meta: meta(at(2026, 2, 1), None),
            data: IssuedByData {
                details: "planted".to_string(),
            },
        },
    }
}

/// Real direction (`planning/mapper.rs`): src = ward, dst = contract.
fn represents(ward: &str, contract: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(ward.to_string()),
        dst: EntityId(contract.to_string()),
        weight: EdgeWeight::Represents {
            meta: meta(at(2026, 2, 1), None),
            data: RepresentsData {
                details: "planted".to_string(),
            },
        },
    }
}

/// Real direction (`ch_disqualified.rs`): src = officer, dst = disqualification node.
fn disqualified_from(officer: &str, disq: &str) -> PlantedEdge {
    let from = at(2025, 1, 1);
    let until = at(2030, 1, 1);
    PlantedEdge {
        src: EntityId(officer.to_string()),
        dst: EntityId(disq.to_string()),
        weight: EdgeWeight::DisqualifiedFrom {
            meta: meta(from, Some(until)),
            data: DisqualifiedFromData {
                disqualified_from: Some(from),
                disqualified_until: Some(until),
                case_number: Some("PLANTED-1".to_string()),
            },
        },
    }
}

fn psc(id: &str, name: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Psc(PscData {
            name: name.to_string(),
            company_number: None,
            natures_of_control: None,
        }),
    )
}

fn property(id: &str, country_incorporated: Option<&str>) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Property(PropertyData {
            title_number: Some("PLANTED-TN1".to_string()),
            tenure: Some("Freehold".to_string()),
            price_paid: Some(250_000.0),
            company_number: None,
            country_incorporated: country_incorporated.map(str::to_string),
        }),
    )
}

fn offshore_entity(id: &str, name: &str, jurisdiction: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: name.to_string(),
            jurisdiction: Some(jurisdiction.to_string()),
            icij_node_id: None,
        }),
    )
}

fn charity(id: &str, name: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Charity(CharityData {
            name: name.to_string(),
            charity_number: None,
            registered_name: None,
        }),
    )
}

/// Real direction (`crates/enricher/src/adapters/ch_psc.rs:452-461`): src = the PSC/beneficial
/// owner, dst = the controlled entity.
fn controls(psc: &str, entity: &str) -> PlantedEdge {
    PlantedEdge {
        src: EntityId(psc.to_string()),
        dst: EntityId(entity.to_string()),
        weight: EdgeWeight::Controls {
            meta: meta(at(2022, 1, 1), None),
        },
    }
}

/// Structure 7 (Phase 49): a `NodeUpdated` status flip for `B`, seeded separately from
/// `topology()` via `events::seed_planted` — a second `NodeAdded` for `B` here would violate
/// `node_ids_are_unique`'s uniqueness invariant. Exported (not private) because `events.rs` calls
/// it directly to build the `NodeUpdated` payload.
///
/// A `NodeUpdated` event replaces the WHOLE node weight, not a diff — `company_number: None`/
/// `incorporation_date: None` here used to silently drop `B`'s real values (`"PL000002"`,
/// `2023-09-01`, both set on its `NodeAdded` in `topology()`), even though this function's only
/// job is to flip `status`. Found by 49-11's full `URBAN_DB_TESTS=1 cargo test --workspace` gate:
/// with `company_number` gone, `crates/workbench/src/entity/profile_map.rs`'s `related_row`
/// cannot reconstruct `B`'s `GB-COH:{number}` pivot id, so `A`'s related row for `B` rendered
/// with `pivot: None` — a real regression in `planted_profile.rs`'s
/// `every_planted_profile_matches_the_manifest` golden, not merely a name mismatch. Mirrors
/// `company()`'s own derivation (`id.trim_start_matches("GB-COH:")`) and `B`'s own
/// `topology()` incorporation date so the flip changes status only.
pub fn entity_in_liquidation(id: &str, name: &str) -> (EntityId, NodeWeight) {
    (
        EntityId(id.to_string()),
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: Some(id.trim_start_matches("GB-COH:").to_string()),
            status: EntityStatus::InLiquidation(at(2026, 1, 1)),
            incorporation_date: Some(day(2023, 9, 1)),
        }),
    )
}

/// The `graph_events.event_id` `events::seed_planted` uses for the `NodeUpdated` row built from
/// [`entity_in_liquidation`].
pub const B_IN_LIQUIDATION_UPDATE_ID: &str = "NodeUpdated-B-inliquidation";

fn set(ids: &[&'static str]) -> BTreeSet<&'static str> {
    ids.iter().copied().collect()
}

/// The six planted structures, as nodes and edges. No expectations live here.
fn topology() -> (Vec<(EntityId, NodeWeight)>, Vec<PlantedEdge>) {
    let mut nodes: Vec<(EntityId, NodeWeight)> = Vec::new();
    let mut edges: Vec<PlantedEdge> = Vec::new();

    // Structure 4 first: ward and buyer, so the reachable set hangs off the ward.
    nodes.push((
        EntityId(WARD.to_string()),
        NodeWeight::Ward(WardData {
            name: "Planted Ward".to_string(),
        }),
    ));
    nodes.push(company(
        BUY,
        "Planted Council",
        EntityStatus::Active,
        day(2020, 1, 1),
    ));
    for (id, name) in [
        (C1, "Planted contract 1"),
        (C2, "Planted contract 2"),
        (C3, "Planted contract 3"),
        (C4, "Planted contract 4"),
    ] {
        nodes.push(contract(id, name));
        edges.push(represents(WARD, id));
        edges.push(issued_by(BUY, id));
    }

    // Structure 1: phoenix chain. `B` is incorporated after `A` dissolves, so the dates carry the
    // pattern rather than merely the topology.
    nodes.push(officer(O1, "Planted Officer One"));
    nodes.push(company(
        A,
        "Planted Alpha Ltd",
        EntityStatus::Dissolved(at(2023, 6, 1)),
        day(2020, 1, 1),
    ));
    nodes.push(company(
        B,
        "Planted Beta Ltd",
        EntityStatus::Active,
        day(2023, 9, 1),
    ));
    nodes.push(location(L1, "1 Planted Street", "PL11AA"));
    edges.push(directs(O1, A));
    edges.push(directs(O1, B));
    edges.push(located_at(A, L1));
    edges.push(located_at(B, L1));
    edges.push(awarded_to(B, C1));

    // Structure 2: address cluster (8 companies at L2, one of them awarded C4).
    nodes.push(location(L2, "2 Planted Street", "PL22BB"));
    for i in 1..=8 {
        let id = l2_company(i);
        nodes.push(company(
            id,
            &format!("Planted Cluster {i} Ltd"),
            EntityStatus::Active,
            day(2020, 1, 1),
        ));
        edges.push(located_at(id, L2));
    }
    edges.push(awarded_to(l2_company(1), C4));

    // Structure 3: two K5,5 blocks plus a bridge officer that splits 2+2 across them. Two bridge
    // edges per side keeps the cross-block credit off any single company: with one edge each side,
    // `q1`/`s1` would sit on every cross pair's shortest path as well as the bridge, and outrank
    // it. Split 2+2, each company carries half the cross credit and only `OB` carries all of it.
    for i in 1..=5 {
        nodes.push(officer(p(i), &format!("Planted Block One Officer {i}")));
        nodes.push(company(
            q(i),
            &format!("Planted Block One Co {i}"),
            EntityStatus::Active,
            day(2020, 1, 1),
        ));
        nodes.push(officer(r(i), &format!("Planted Block Two Officer {i}")));
        nodes.push(company(
            s(i),
            &format!("Planted Block Two Co {i}"),
            EntityStatus::Active,
            day(2020, 1, 1),
        ));
    }
    for i in 1..=5 {
        for j in 1..=5 {
            edges.push(directs(p(i), q(j)));
            edges.push(directs(r(i), s(j)));
        }
    }
    nodes.push(officer(OB, "Planted Bridge Officer"));
    edges.push(directs(OB, q(1)));
    edges.push(directs(OB, q(2)));
    edges.push(directs(OB, s(1)));
    edges.push(directs(OB, s(2)));
    edges.push(awarded_to(q(1), C2));
    edges.push(awarded_to(s(1), C3));

    // Structure 5: disqualified director in block 1.
    nodes.push(officer(OD, "Planted Disqualified Officer"));
    nodes.push((
        EntityId(DQ1.to_string()),
        NodeWeight::Disqualification(DisqualificationData {
            case_number: Some("PLANTED-1".to_string()),
            disqualified_from: Some(at(2025, 1, 1)),
            disqualified_until: Some(at(2030, 1, 1)),
        }),
    ));
    edges.push(directs(OD, q(2)));
    edges.push(disqualified_from(OD, DQ1));

    // Structure 7: PSC control + OSINT node-type coverage (Phase 49). `PSC1` controls the
    // existing company `A` (no new company id family, per D-01's minimal-growth guidance), so it
    // is reachable from `WARD` like `A` is. `PROPERTY1`/`OFFSHORE1`/`CHARITY1` are deliberately
    // edgeless — OSINT node-type coverage only, not reachability or motif coverage.
    nodes.push(psc(PSC1, "Planted Beneficial Owner"));
    nodes.push(property(PROPERTY1, Some("Jersey")));
    nodes.push(offshore_entity(OFFSHORE1, "Planted Offshore Ltd", "Jersey"));
    nodes.push(charity(CHARITY1, "Planted Charity Trust"));
    edges.push(controls(PSC1, A));

    // Structure 8 (Phase 50 plan 10, gap closure): term/marker coverage for six previously-
    // DataGap toy-analysis probes. None of the three new pieces are connected by any edge to the
    // WARD-reachable component — deliberately isolated, like `PROPERTY1`/`OFFSHORE1`/`CHARITY1`,
    // so no existing reachable-set/community/k-core/betweenness/profile/disambiguation literal is
    // disturbed by this structure.
    nodes.push(planning_contract(
        PLANNING_C1,
        "Planted Planning Contract 1",
        Some(ApplicantProvenance::Published),
    ));
    nodes.push(political_actor(
        COUNCIL_PA1,
        "Planted Councillor One",
        Some("Planted Council"),
    ));
    nodes.push(company(
        COUNCIL_INTEREST_CO,
        "Planted Interest Holdings Ltd",
        EntityStatus::Active,
        day(2020, 1, 1),
    ));
    edges.push(declares_interest(COUNCIL_PA1, COUNCIL_INTEREST_CO));

    // Structure 6: noise, unreachable.
    for i in 1..=NOISE_PAIRS {
        nodes.push(officer(
            &noise_officer(i),
            &format!("Planted Noise Officer {i}"),
        ));
        nodes.push(company(
            &noise_company(i),
            &format!("Planted Noise Co {i}"),
            EntityStatus::Active,
            day(2020, 1, 1),
        ));
        edges.push(directs(&noise_officer(i), &noise_company(i)));
    }

    (nodes, edges)
}

/// Every answer the fixture is built to give, as literals. Never computed from `topology()`.
fn manifest() -> Manifest {
    let block_one = [P[0], P[1], P[2], P[3], P[4], Q[0], Q[1], Q[2], Q[3], Q[4]];
    let block_two = [R[0], R[1], R[2], R[3], R[4], S[0], S[1], S[2], S[3], S[4]];

    // `OD`/`DQ1` are deliberately NOT in block one's community. graphrs' move gain (its
    // `Modularity::q`, resolution 1.0, m = 62 default-selection edges in scope) is
    // q = k_in/62 − k_i·Σ_tot/7688, applied only when strictly positive. At level 0, `OD`
    // (degree 2) pairing with `DQ1` scores 1/62 − 2·1/7688 = +0.0159, so the pair forms first.
    // At the aggregated level the `{OD, DQ1}` super-node (degree 3) joining block one scores
    // 1/62 − 3·Σ_tot/7688 < 0 for both Σ_tot = 55 (`OB` in block two) and 59 (`OB` in block
    // one). Louvain therefore leaves them as a 2-node community, which `min_community_size = 5`
    // then hides. Measured against `run_algorithm` 2026-09-09.
    let mut community_one = set(&block_one);
    community_one.insert(C2);
    let mut community_two = set(&block_two);
    community_two.insert(C3);

    let mut l2_cluster = set(&L2_COMPANIES);
    l2_cluster.insert(L2);
    l2_cluster.insert(C4);

    let mut k5_core = set(&block_one);
    k5_core.extend(block_two);

    Manifest {
        // Phase 49's Structure 7 adds 4 nodes (PSC1, PROPERTY1, OFFSHORE1, CHARITY1) and 1 edge
        // (controls(PSC1, A)). PSC1 attaches to the already-reachable `A`, so it and its edge join
        // the reachable set too; PROPERTY1/OFFSHORE1/CHARITY1 are edgeless and stay unreachable
        // (see `noise_is_exactly_the_unreachable_remainder`'s updated `+ 3`).
        //
        // Phase 50 plan 10's Structure 8 adds 3 more nodes (PLANNING_C1, COUNCIL_PA1,
        // COUNCIL_INTEREST_CO) and 1 edge (declares_interest(COUNCIL_PA1, COUNCIL_INTEREST_CO)),
        // all deliberately edgeless-to-WARD, so `seeded_nodes`/`seeded_edges` grow while
        // `reachable_nodes`/`reachable_edges` stay unchanged — re-measured live via
        // `cargo test -p urban-integrity-test-support`, not hand-computed (T-50-10).
        seeded_nodes: 89,
        seeded_edges: 102,
        reachable_nodes: 43,
        reachable_edges: 81,
        ward_code: WARD_CODE,
        lpa_code: LPA_CODE,
        shortest_paths: vec![
            ExpectedPath {
                from: A,
                to: B,
                edge_types_included: DEFAULT_EDGES,
                node_ids: vec![A, O1, B],
            },
            ExpectedPath {
                from: C1,
                to: C2,
                edge_types_included: DEFAULT_PLUS_ISSUED_BY,
                node_ids: vec![C1, BUY, C2],
            },
            ExpectedPath {
                from: C1,
                to: C2,
                edge_types_included: DEFAULT_PLUS_REPRESENTS,
                node_ids: vec![C1, WARD, C2],
            },
        ],
        no_path: vec![ExpectedNoPath {
            from: C1,
            to: C2,
            edge_types_included: DEFAULT_EDGES,
        }],
        all_simple_paths: vec![
            ExpectedPathSet {
                from: A,
                to: B,
                edge_types_included: DEFAULT_PLUS_LOCATED_AT,
                hop_cap: 4,
                count: 2,
            },
            ExpectedPathSet {
                from: A,
                to: B,
                edge_types_included: DEFAULT_EDGES,
                hop_cap: 4,
                count: 1,
            },
        ],
        communities: vec![
            ExpectedCommunitySet {
                edge_types_included: DEFAULT_EDGES,
                seed: 42,
                min_size: 5,
                // Phase 49's Structure 7 adds a fifth member (`PSC1`, via the `Controls` edge —
                // `Controls` is already in `DEFAULT_EDGES`) to the phoenix chain component
                // {O1, A, B, C1}, which previously sat at 4 (below `min_size`) and was folded
                // into the hidden remainder. At exactly 5 it is now kept (`seeded_louvain` keeps
                // `>= min_size`) and appears as its own community.
                sets: vec![
                    community_one.clone(),
                    community_two.clone(),
                    set(&[O1, A, B, C1, PSC1]),
                ],
                flexible: vec![OB],
                // 43 reachable − (11 + 11 + 5 + OB) = 15
                hidden_count: 15,
            },
            ExpectedCommunitySet {
                edge_types_included: DEFAULT_PLUS_LOCATED_AT,
                seed: 42,
                min_size: 5,
                // `LocatedAt` turns the address cluster into one community of 10 and the phoenix
                // chain into one of 6 (`PSC1` via `Controls`, `L1` via `LocatedAt`; kept:
                // `seeded_louvain` keeps `>= min_size`). The two blocks still split as above.
                // Only the ward and buyer stay isolated.
                sets: vec![
                    community_one,
                    community_two,
                    l2_cluster,
                    set(&[A, B, O1, L1, C1, PSC1]),
                ],
                flexible: vec![OB],
                // 43 reachable − (11 + 11 + 10 + 6 + OB) = 4 (the ward, the buyer, `OD`, `DQ1`)
                hidden_count: 4,
            },
        ],
        k_cores: vec![
            ExpectedKCore {
                k: 5,
                edge_types_included: DEFAULT_EDGES,
                // Exactly the 20 block nodes: every p/q/r/s has degree 5 inside its own K5,5.
                // OB (degree 4) excluded, and every degree-1 attachment (C2, C3) peels, taking
                // OD (degree 2 via q2 and DQ1) and DQ1 with it.
                members: k5_core,
            },
            ExpectedKCore {
                k: 2,
                edge_types_included: DEFAULT_PLUS_LOCATED_AT,
                // L2 cluster is a star plus one pendant contract: fully peeled at k=2. The two
                // blocks and OB survive (OB has degree 4). Phoenix chain: A(2: O1, L1), B(3),
                // O1(2), L1(2), C1(1 → peeled) → A,B,O1,L1 survive as a 4-cycle. 20 + 1 + 4 = 25.
                members: {
                    let mut m = set(&block_one);
                    m.extend(block_two);
                    m.extend([OB, A, B, O1, L1]);
                    m
                },
            },
        ],
        betweenness: vec![ExpectedBetweenness {
            edge_types_included: DEFAULT_EDGES,
            top_1: OB,
            // Frozen from `run_algorithm`'s own output 2026-09-09. This is the code's number, not
            // the textbook's: on 42 in-scope nodes every node is a pivot (exact, not sampled), and
            // `sampled_betweenness` sums per-source deltas WITHOUT the undirected halving — so the
            // frozen value is exactly 2× the textbook 144.333. Freeze what the code returns; a
            // change here means the implementation changed, which is the point of the assertion.
            top_1_score: 288.666_666_666_666_7,
        }],
        ward_subgraphs: vec![ExpectedWardSubgraph {
            ward_id: WARD,
            members: set(&[WARD, C1, C2, C3, C4]),
        }],
        side_card_sample: SideCardSample {
            id: OB,
            degree: 4,
            earliest_from: "2021-01-01",
            latest_to: None,
        },
        profiles: vec![
            // Structure 1, the dissolved predecessor. One officer, one address, no contract —
            // the phoenix pattern's "before" half, and the only subject here whose header does
            // not read Active.
            ExpectedProfile {
                id: A,
                name: "Planted Alpha Ltd",
                status: "Dissolved",
                sections: EMPTY_EXCEPT_OFFICERS_ADDRESSES_RELATED,
                related_ids: &[B],
            },
            // Structure 1, the successor. `A`'s dissolution and `B`'s contract are the phoenix
            // pattern's "before"/"after" halves; Phase 49's Structure 7 layers a second signal on
            // top — a `NodeUpdated` flips `B` to `InLiquidation` (2026-01-01), so the status word
            // this profile now shows is neither of the two `A` can show (`Dissolved`) nor the
            // plain `Active` a phoenix successor would have before Structure 7 existed. Re-derived
            // here (49-11) from the actual `status_word()` the profile route now returns, not
            // hand-guessed — see `entity_in_liquidation`'s own doc comment for why this manifest
            // entry had gone stale (it still said `"Active"` after 49-02 added the flip).
            ExpectedProfile {
                id: B,
                name: "Planted Beta Ltd",
                status: "InLiquidation",
                sections: &[
                    ("officers", 1),
                    ("addresses", 1),
                    ("contracts", 1),
                    ("planning_applications", 0),
                    ("property", 0),
                    ("osint_links", 0),
                    ("beneficial_owners", 0),
                    ("disqualifications", 0),
                    ("trustee_of", 0),
                    ("donates_to", 0),
                    ("offshore_links", 0),
                    ("related", 1),
                ],
                related_ids: &[A],
            },
            // Structure 3, block one, company one: the densest officer list in the fixture, and
            // the only subject whose Related set crosses into the other block — through `OB`,
            // which is the bridge the betweenness expectation above ranks first.
            ExpectedProfile {
                id: Q[0],
                name: "Planted Block One Co 1",
                status: "Active",
                sections: &[
                    ("officers", 6),
                    ("addresses", 0),
                    ("contracts", 1),
                    ("planning_applications", 0),
                    ("property", 0),
                    ("osint_links", 0),
                    ("beneficial_owners", 0),
                    ("disqualifications", 0),
                    ("trustee_of", 0),
                    ("donates_to", 0),
                    ("offshore_links", 0),
                    ("related", 6),
                ],
                related_ids: &[Q[1], Q[2], Q[3], Q[4], S[0], S[1]],
            },
            // Structure 2, the address cluster: **no officer at all** and seven relatives, every
            // one of them reached by the shared address rather than by a shared director. The
            // one subject here that proves Related is not a synonym for "companies my directors
            // also direct".
            ExpectedProfile {
                id: L2_COMPANIES[0],
                name: "Planted Cluster 1 Ltd",
                status: "Active",
                sections: &[
                    ("officers", 0),
                    ("addresses", 1),
                    ("contracts", 1),
                    ("planning_applications", 0),
                    ("property", 0),
                    ("osint_links", 0),
                    ("beneficial_owners", 0),
                    ("disqualifications", 0),
                    ("trustee_of", 0),
                    ("donates_to", 0),
                    ("offshore_links", 0),
                    ("related", 7),
                ],
                related_ids: &[
                    L2_COMPANIES[1],
                    L2_COMPANIES[2],
                    L2_COMPANIES[3],
                    L2_COMPANIES[4],
                    L2_COMPANIES[5],
                    L2_COMPANIES[6],
                    L2_COMPANIES[7],
                ],
            },
            // The buyer that issued all four contracts, and whose Contracts section is
            // **empty** — the read path counts `AwardedTo` edges where the subject is the `src`,
            // and a buyer is the `src` of `IssuedBy`, never of `AwardedTo`. An entirely blank
            // profile for the busiest node in structure 4. This is the fixture's one deliberate
            // trap: it is the shape a reader is most likely to call a bug, and it is correct.
            ExpectedProfile {
                id: BUY,
                name: "Planted Council",
                status: "Active",
                sections: ALL_EMPTY,
                related_ids: &[],
            },
        ],
        as_at_profiles: vec![
            // Before anything: `B`'s directorship and address both begin 2021-01-01 and its
            // contract 2026-02-01, so at this date every row the screen holds is positively
            // outside — and none is removed. A section that looks full and reads entirely muted
            // is the shape PIV-04's "marks, never deletes" rule produces, and it exists nowhere
            // on the live corpus.
            ExpectedAsAtProfile {
                id: B,
                as_at: "2020-06-30",
                sections: &[
                    SectionTally {
                        section: "officers",
                        in_force: 0,
                        out_of_force: 1,
                        undated: 0,
                    },
                    SectionTally {
                        section: "addresses",
                        in_force: 0,
                        out_of_force: 1,
                        undated: 0,
                    },
                    SectionTally {
                        section: "contracts",
                        in_force: 0,
                        out_of_force: 1,
                        undated: 0,
                    },
                ],
            },
            // The one date that splits the screen: the directorship and the address are in
            // force, the contract is not yet. Two chips on one page, which is the whole of what
            // the as-at control is for.
            ExpectedAsAtProfile {
                id: B,
                as_at: "2022-06-30",
                sections: &[
                    SectionTally {
                        section: "officers",
                        in_force: 1,
                        out_of_force: 0,
                        undated: 0,
                    },
                    SectionTally {
                        section: "addresses",
                        in_force: 1,
                        out_of_force: 0,
                        undated: 0,
                    },
                    SectionTally {
                        section: "contracts",
                        in_force: 0,
                        out_of_force: 1,
                        undated: 0,
                    },
                ],
            },
            // After everything began: all in force. Distinct from an unfiltered profile, whose
            // rows are `NotFiltered` — an unfiltered screen has not established that anything
            // was in force at any date, and the chips say so.
            ExpectedAsAtProfile {
                id: B,
                as_at: "2026-06-30",
                sections: &[
                    SectionTally {
                        section: "officers",
                        in_force: 1,
                        out_of_force: 0,
                        undated: 0,
                    },
                    SectionTally {
                        section: "addresses",
                        in_force: 1,
                        out_of_force: 0,
                        undated: 0,
                    },
                    SectionTally {
                        section: "contracts",
                        in_force: 1,
                        out_of_force: 0,
                        undated: 0,
                    },
                ],
            },
        ],
        disambiguations: vec![
            // One match: the list still renders, because "exactly one" is an answer the analyst
            // must be able to see rather than be silently redirected past.
            ExpectedDisambiguation {
                query: "Planted Alpha",
                rows: 1,
                more_may_exist: false,
            },
            // Five: the block-one companies. The five block-one *officers* share the phrase
            // "Planted Block One" and do **not** appear — `query_entity_by_name` matches
            // `NodeWeight::Entity` only, the same restriction as the profile route's 404 policy.
            ExpectedDisambiguation {
                query: "Planted Block One",
                rows: 5,
                more_may_exist: false,
            },
            // Eight: the whole address cluster, below the cap.
            ExpectedDisambiguation {
                query: "Planted Cluster",
                rows: 8,
                more_may_exist: false,
            },
            // Forty-one companies carry "Planted"; the read path's ten-match cap bits and says
            // so. The cap being *reported* is the expectation here, not the ten rows.
            ExpectedDisambiguation {
                query: "Planted",
                rows: 10,
                more_may_exist: true,
            },
        ],
        neighbourhoods: vec![
            // One hop from the dissolved predecessor: its officer, its address, and (Phase 49)
            // the beneficial owner controlling it via the `Controls` edge — `bfs_traversal` walks
            // `EdgeAdded` events in both directions regardless of type, so `PSC1` is a genuine
            // 1-hop neighbour of `A`. Three neighbours is still small enough that a node cap of 1
            // has an answer a reader can check without arithmetic — which is the point, because
            // the cap clause is the thing under test, not the traversal.
            ExpectedNeighbourhood {
                id: A,
                hops: 1,
                total: 3,
                node_ids: &[O1, L1, PSC1],
            },
        ],
        // Phase 49 (EVM-05, plan 49-11): re-verified against 49-03..49-09's SUMMARYs and each
        // probe's own `run()` source before being written as literals here — never hand-guessed.
        // All seven counts are well below every probe's own `DISPLAY_CAP` (8), so none of these
        // totals are capped on this fixture.
        evidence_refs: vec![
            ExpectedProbeRefs {
                // The one confirmed `ch_psc` identity_links row (PSC1 -> A): one target-node ref.
                probe_name: "psc-beneficial-ownership",
                ref_count: 1,
            },
            ExpectedProbeRefs {
                // Phase 50 plan 2: disqualified_director_edges.rs was widened from two flat
                // src_id-only lists to one EvidenceRow per officer, refs[0]=officer,
                // refs[1..]=that officer's distinct targets. On this fixture: DisqualifiedFrom
                // officer OD -> target DQ1 (2 refs) + Controls officer PSC1 -> target A (2 refs)
                // = 4 refs total. Re-measured against a live widened run, not predicted.
                probe_name: "osint-risk-edges",
                ref_count: 4,
            },
            ExpectedProbeRefs {
                // All three confirmed/auto-linked identity_links rows, deduped by
                // target_node_id: A (ch_psc), B (electoral_commission), and (Phase 50 plan 10)
                // entity:planted-flip-applicant (land_registry_ccod, the land-flip gate-3
                // corroboration link) — re-measured live against the extended fixture, not
                // predicted from the plan text.
                probe_name: "identity-link-registry",
                ref_count: 3,
            },
            ExpectedProbeRefs {
                // The one planted PhoenixLoop MotifDetected event's 3 distinct participants
                // (O1, A, B).
                probe_name: "precomputed-alerts",
                ref_count: 3,
            },
            ExpectedProbeRefs {
                // The one Property node (PROPERTY1).
                probe_name: "property-nodes",
                ref_count: 1,
            },
            ExpectedProbeRefs {
                // The one confirmed electoral_commission identity_links row (target B). No
                // DonatesTo edge is planted in this fixture, so the donor-side ref step adds 0.
                probe_name: "phase20-ec-donations",
                ref_count: 1,
            },
            ExpectedProbeRefs {
                // 1 Psc + 1 Charity + 1 OffshoreEntity + 1 InLiquidation (B's status flip);
                // Property stays unrefed in this probe (property_nodes.rs owns that ref).
                probe_name: "osint-new-node-types",
                ref_count: 4,
            },
            ExpectedProbeRefs {
                // Phase 50 plan 8, criterion 2's fixed answer: the L2 star-cluster evidence
                // row's own ref count (1 centre + 8 leaves), NOT the sum of every evidence row
                // this probe emits. Unlike the seven entries above, this probe's evidence is not
                // a single flat list: `evidence[0]` is an aggregate row with no refs, and this
                // fixture happens to seed a SECOND, incidental shared-address cluster at `L1`
                // (structure 1's phoenix-loop predecessor/successor, which also shares a
                // registered address, contributing 3 more refs) alongside the intended `L2`
                // star. Measured live against the seeded fixture 2026-09-17: evidence =
                // [aggregate (0 refs), L2 row (9 refs, biggest cluster first), L1 row (3 refs)],
                // total sum = 12. `crates/workbench/tests/finding_star_golden.rs` and this
                // file's companion test both locate the star row by `refs[0].id == L2`, not by
                // array index or total-refs sum, and assert against this literal.
                probe_name: "address-cluster-exact",
                ref_count: 9,
            },
        ],
    }
}

/// `ExpectedProfile::sections` for a subject with nothing on it at all — see [`BUY`]'s entry.
const ALL_EMPTY: &[SectionCount] = &[
    ("officers", 0),
    ("addresses", 0),
    ("contracts", 0),
    ("planning_applications", 0),
    ("property", 0),
    ("osint_links", 0),
    ("beneficial_owners", 0),
    ("disqualifications", 0),
    ("trustee_of", 0),
    ("donates_to", 0),
    ("offshore_links", 0),
    ("related", 0),
];

/// `ExpectedProfile::sections` for the dissolved predecessor `A`.
const EMPTY_EXCEPT_OFFICERS_ADDRESSES_RELATED: &[SectionCount] = &[
    ("officers", 1),
    ("addresses", 1),
    ("contracts", 0),
    ("planning_applications", 0),
    ("property", 0),
    ("osint_links", 0),
    ("beneficial_owners", 0),
    ("disqualifications", 0),
    ("trustee_of", 0),
    ("donates_to", 0),
    ("offshore_links", 0),
    ("related", 1),
];

/// Build the fixture. Deterministic; every call returns identical output.
pub fn planted_graph() -> PlantedGraph {
    let (nodes, edges) = topology();
    PlantedGraph {
        nodes,
        edges,
        manifest: manifest(),
        identity_links: vec![
            // Feeds `psc-beneficial-ownership` and `identity-link-registry`.
            PlantedIdentityLink {
                key: "planted-psc1-a",
                source_node_id: PSC1,
                target_node_id: A,
                confidence: 0.95,
                source: "ch_psc",
                status: "confirmed",
            },
            // Feeds `phase20-ec-donations` and adds a second adapter to
            // `identity-link-registry`'s top-adapters distribution.
            PlantedIdentityLink {
                key: "planted-o1-b-ec",
                source_node_id: O1,
                target_node_id: B,
                confidence: 0.85,
                source: "electoral_commission",
                status: "confirmed",
            },
            // Phase 50 plan 10: the corroborating link `has_gate3_corroboration` reads for the
            // planted land-flip survivor below (`land_flip_capability.rs`'s `property:`-prefixed,
            // confidence>=0.70, auto_linked/confirmed predicate). `target_node_id` is a distinct
            // id from `A`/`B`, so `identity-link-registry`'s dedup-by-target-node-id ref count
            // grows by exactly 1 (re-measured live in Task 2, not predicted here).
            PlantedIdentityLink {
                key: "planted-flip-applicant-ccod",
                source_node_id: "property:planted-flip-title1",
                target_node_id: "entity:planted-flip-applicant",
                confidence: 1.0,
                source: "land_registry_ccod",
                status: "auto_linked",
            },
        ],
        // `ch_disqualified`'s Clear-vs-DataGap branch is already exercised by Structure 5's
        // pre-existing `DisqualifiedFrom` edge, so it is not repeated here. Phase 50 plan 10 adds
        // `ch_disqualified` (the marker alone, paired with Structure 5's real `DisqualifiedFrom`
        // edge, closes `osint-evasion-alerts`/`osint-disqualified-directors` via
        // `Cause::NoRegisterOverlap`) and `council_registers` (the marker `phase20-council-
        // registers` gates on first, before either count query).
        enrichment_run_sources: vec![
            "ch_psc",
            "electoral_commission",
            "land_registry_ocod",
            "ch_disqualified",
            "council_registers",
        ],
        motif_events: vec![PlantedMotifEvent {
            event_id: "MotifDetected-planted-phoenix1",
            motif_type: "PhoenixLoop",
            participants: &[O1, A, B],
            risk_score: 0.85,
        }],
        // Phase 50 plan 10: population = applications_scanned - gate1_no_ppd_match = 1,
        // candidate_apps = 0 -> matches = 0, capable (via the identity_links row above) = 1 ->
        // capable == population -> Verdict::classify(1, 1, 0, 1, "") -> Clear/Info, per this
        // plan's `<interfaces>` block.
        land_flip_run: PlantedLandFlipRun {
            applications_scanned: 1,
            gate1_no_ppd_match: 0,
            candidate_apps: 0,
            survivors: vec![PlantedLandFlipSurvivor {
                planning_ref: "PLANTED-FLIP/0001",
                applicant_id: "entity:planted-flip-applicant",
                location_id: "location:planted-flip-loc1",
            }],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// The variant name as the UI and the manifest spell it. A `match` rather than a
    /// serialisation round-trip, so no extra dependency is needed.
    fn edge_type_name(w: &EdgeWeight) -> &'static str {
        match w {
            EdgeWeight::Directs { .. } => "Directs",
            EdgeWeight::LocatedAt { .. } => "LocatedAt",
            EdgeWeight::AwardedTo { .. } => "AwardedTo",
            EdgeWeight::IssuedBy { .. } => "IssuedBy",
            EdgeWeight::Represents { .. } => "Represents",
            EdgeWeight::Controls { .. } => "Controls",
            EdgeWeight::TrusteeOf { .. } => "TrusteeOf",
            EdgeWeight::OwnsProperty { .. } => "OwnsProperty",
            EdgeWeight::OffshoreLink { .. } => "OffshoreLink",
            EdgeWeight::DonatesTo { .. } => "DonatesTo",
            EdgeWeight::DisqualifiedFrom { .. } => "DisqualifiedFrom",
            EdgeWeight::DeclaresInterest { .. } => "DeclaresInterest",
        }
    }

    #[test]
    fn builder_is_deterministic() {
        let a = planted_graph();
        let b = planted_graph();
        assert_eq!(a.nodes, b.nodes);
        assert_eq!(a.edges, b.edges);
        assert_eq!(a.manifest, b.manifest);
    }

    #[test]
    fn counts_match_manifest() {
        let g = planted_graph();
        assert_eq!(g.nodes.len(), g.manifest.seeded_nodes);
        assert_eq!(g.edges.len(), g.manifest.seeded_edges);
    }

    #[test]
    fn every_edge_endpoint_is_a_seeded_node() {
        let g = planted_graph();
        let ids: BTreeSet<&str> = g.nodes.iter().map(|(id, _)| id.0.as_str()).collect();
        for e in &g.edges {
            assert!(ids.contains(e.src.0.as_str()), "missing src {}", e.src.0);
            assert!(ids.contains(e.dst.0.as_str()), "missing dst {}", e.dst.0);
        }
    }

    #[test]
    fn node_ids_are_unique() {
        let g = planted_graph();
        let ids: BTreeSet<&str> = g.nodes.iter().map(|(id, _)| id.0.as_str()).collect();
        assert_eq!(ids.len(), g.nodes.len());
    }

    #[test]
    fn bridge_officer_has_two_edges_into_each_block() {
        let g = planted_graph();
        let mut ob_targets: Vec<&str> = g
            .edges
            .iter()
            .filter(|e| e.src.0 == OB)
            .map(|e| e.dst.0.as_str())
            .collect();
        ob_targets.sort_unstable();
        let mut expected = vec![Q[0], Q[1], S[0], S[1]];
        expected.sort_unstable();
        assert_eq!(ob_targets, expected);
    }

    #[test]
    fn noise_is_exactly_the_unreachable_remainder() {
        let g = planted_graph();
        // Structure 6 contributes 2*NOISE_PAIRS unreachable nodes. Structure 7 (Phase 49) adds
        // three more that are deliberately isolated rather than noise pairs: PROPERTY1,
        // OFFSHORE1, CHARITY1. PSC1 is NOT counted here — it connects to `A` via the Controls
        // edge and is therefore reachable, unlike its Structure-7 siblings. Structure 8 (Phase 50
        // plan 10) adds three more isolated nodes: PLANNING_C1 (edgeless) and the
        // COUNCIL_PA1/COUNCIL_INTEREST_CO pair (edged to each other only, never to WARD).
        assert_eq!(
            g.manifest.seeded_nodes - g.manifest.reachable_nodes,
            2 * NOISE_PAIRS + 6
        );
    }

    #[test]
    fn every_manifest_id_is_a_seeded_node() {
        let g = planted_graph();
        let seeded: BTreeSet<&str> = g.nodes.iter().map(|(id, _)| id.0.as_str()).collect();
        let m = &g.manifest;

        let mut referenced: Vec<&str> = Vec::new();
        for p in &m.shortest_paths {
            referenced.push(p.from);
            referenced.push(p.to);
            referenced.extend(p.node_ids.iter().copied());
        }
        for n in &m.no_path {
            referenced.push(n.from);
            referenced.push(n.to);
        }
        for p in &m.all_simple_paths {
            referenced.push(p.from);
            referenced.push(p.to);
        }
        for c in &m.communities {
            for s in &c.sets {
                referenced.extend(s.iter().copied());
            }
            referenced.extend(c.flexible.iter().copied());
        }
        for k in &m.k_cores {
            referenced.extend(k.members.iter().copied());
        }
        for b in &m.betweenness {
            referenced.push(b.top_1);
        }
        for w in &m.ward_subgraphs {
            referenced.push(w.ward_id);
            referenced.extend(w.members.iter().copied());
        }
        referenced.push(m.side_card_sample.id);

        assert!(
            !referenced.is_empty(),
            "manifest referenced no ids at all — the collection above went stale"
        );
        for id in referenced {
            assert!(seeded.contains(id), "manifest names unseeded node {id}");
        }
    }

    #[test]
    fn reachable_counts_match_an_undirected_walk_from_the_ward() {
        let g = planted_graph();
        let mut adj: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for e in &g.edges {
            adj.entry(e.src.0.as_str()).or_default().push(&e.dst.0);
            adj.entry(e.dst.0.as_str()).or_default().push(&e.src.0);
        }

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        seen.insert(WARD);
        queue.push_back(WARD);
        while let Some(cur) = queue.pop_front() {
            for next in adj.get(cur).into_iter().flatten() {
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }

        assert_eq!(seen.len(), g.manifest.reachable_nodes);
        let reachable_edges = g
            .edges
            .iter()
            .filter(|e| seen.contains(e.src.0.as_str()) && seen.contains(e.dst.0.as_str()))
            .count();
        assert_eq!(reachable_edges, g.manifest.reachable_edges);
    }

    #[test]
    fn default_plus_sets_are_default_plus_exactly_one() {
        for (set, extra) in [
            (DEFAULT_PLUS_LOCATED_AT, "LocatedAt"),
            (DEFAULT_PLUS_ISSUED_BY, "IssuedBy"),
            (DEFAULT_PLUS_REPRESENTS, "Represents"),
        ] {
            assert_eq!(set.len(), DEFAULT_EDGES.len() + 1, "{extra} set is not 9");
            assert_eq!(
                &set[..8],
                DEFAULT_EDGES,
                "{extra} set does not open with the defaults"
            );
            assert_eq!(set[8], extra);
        }
    }

    #[test]
    fn no_duplicate_edge_triples() {
        let g = planted_graph();
        let mut seen: BTreeSet<(&str, &str, &str)> = BTreeSet::new();
        for e in &g.edges {
            let triple = (
                e.src.0.as_str(),
                e.dst.0.as_str(),
                edge_type_name(&e.weight),
            );
            assert!(seen.insert(triple), "duplicate edge {triple:?}");
        }
        assert_eq!(seen.len(), g.edges.len());
    }
}
