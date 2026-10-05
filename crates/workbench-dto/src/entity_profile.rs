//! `EntityProfileDto` — the wasm-safe projection of
//! `crates/core::persistence::queries::EntityProfile` (PIV-02, 45-CONTEXT D-01).
//!
//! # The anchor, and what this type is allowed to be
//!
//! D-01 settles it: `EntityProfile` (`queries.rs:259`) is the server-side anchor, already consumed
//! by the CLI `entity` command and by GraphQL, and any gap is closed *there* so all three
//! consumers gain it. This module is that type's view projection and nothing more — it assembles
//! nothing, queries nothing, and must never become a second profile assembler living in the
//! workbench crates. **It holds no SurrealQL and no knowledge of how a profile is built** (PIV-06).
//!
//! # Field-for-field correspondence with `EntityProfile`
//!
//! Every field of the anchor is mapped below. This table is the drift check: a field that appears
//! on `EntityProfile` and in neither column of this table is a section the profile screen cannot
//! render.
//!
//! | `EntityProfile` field | Here |
//! |---|---|
//! | `id`, `name` | [`EntityProfileDto::id`] / [`EntityProfileDto::name`] — the subject of the page |
//! | `status` | [`EntityProfileDto::status`], as a [`StatusFactView`] so the claim carries provenance |
//! | `ch_number`, `incorporation_date` | [`EntityProfileDto::identifiers`] |
//! | `addresses` | [`EntityProfileDto::addresses`] |
//! | `directors` | [`EntityProfileDto::officers`] — "Officers" is the section name in the phase 45 UI spec |
//! | `contracts` | [`EntityProfileDto::contracts`] **and** [`EntityProfileDto::planning_applications`] (D-03) |
//! | `owns_property` | [`EntityProfileDto::property`] |
//! | `osint_links` | [`EntityProfileDto::osint_links`] |
//! | `beneficial_owners` | [`EntityProfileDto::beneficial_owners`] |
//! | `disqualifications` | [`EntityProfileDto::disqualifications`] |
//! | `trustee_of`, `donates_to`, `offshore_links` | the three same-named [`LinkedRecordRowView`] sections |
//! | `sanctions_status` | [`EntityProfileDto::sanctions`] |
//! | `alerts`, `osint_alerts` | [`FindingsSectionView`] |
//! | `related_entities` | [`EntityProfileDto::related`] |
//! | `lpa_presence` | [`EntityProfileDto::lpa_presence`] |
//! | `sub_candidate_links` | **deliberately absent** — see below |
//!
//! Two sections here have no anchor field *yet*: `planning_applications` (plan 45-03 splits it out
//! of `contracts` by destination prefix, D-03) and `findings` (plan 45-05 builds the
//! `RecordRef`→entity reverse index, D-05). They are declared now because this plan is the
//! contract the rest of the phase is built against; a section added to the DTO later would be a
//! section the handler and the renderer were written without.
//!
//! # Why `sub_candidate_links` does not cross this boundary
//!
//! `EntityProfile::sub_candidate_links` carries 0.50–0.69 confidence links and is annotated
//! `#[graphql(skip)]` with "must never appear in GraphQL schema. Do not remove this annotation"
//! (D-13/D-14, ENR-11). The workbench is a new surface for exactly the same rule: a candidate link
//! an analyst has not reviewed must not be rendered beside confirmed facts, where it reads as one.
//! It is left out of the wire vocabulary entirely rather than filtered downstream — and
//! [`crate::provenance::LinkTier`] enforces the same thing one level down, having no
//! `pending_review` variant to construct.
//!
//! # Provenance is mandatory on every row
//!
//! Every row type below carries `provenance: Provenance`, never `Option<Provenance>` (PIV-03,
//! D-04). The case a producer cannot attribute is [`Provenance::Unattributed`] — present, visible
//! and carrying its reason. The two things that are *not* rows carry no provenance and say why:
//! [`LpaPresenceView`] is a count spanning two datasets, and [`FindingsSectionView`]'s probe
//! counts are a property of the probe set rather than of this entity. Attributing either to one
//! source would be the placeholder provenance D-04 forbids.
//!
//! # Pivot targets
//!
//! Every row that can be pivoted into carries `pivot: Option<EntityRef>` (PIV-03's "every
//! neighbour, contract, address and link is a click-through"). [`EntityRef`] is reused rather than
//! a new target type being invented, and D-06 landed with no change to this boundary, as 45-01
//! predicted. `None` means "this row has no pivot target" — the phase 45 UI spec renders those as plain
//! text, not a dead link — and it is `required_option`, so a producer that forgets the key gets an
//! error rather than a silently unclickable row.
//!
//! What 45-01 got wrong is *which* vocabulary `kind` carries. It said "the `RecordKind` name";
//! the producers put `crates/workbench`'s `node_kind_name` in it on the search, OSINT-link and
//! subject-header paths, and have since 44-06. D-06 resolved the collision in the direction of the
//! existing data rather than the existing comment: `RecordKind`'s new variants are spelled
//! `Contract` and `Location`, the names `node_kind_name` already uses for those node classes, so
//! one node class has one spelling on this field whichever producer filled it.

use serde::{Deserialize, Serialize};

use crate::entity_ref::EntityRef;
use crate::paging::Page;
use crate::provenance::Provenance;
use crate::temporal::Temporal;
use crate::verdict_view::VerdictView;

/// Copied verbatim from `population_meta::required_option` (see that module's doc comment for the
/// full rationale) — this crate's established convention is each DTO module owns its own copy,
/// never a shared import.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Wire mirror of `schema::types::node::EntityStatus`. Tagged, and the dissolution/liquidation
/// date is kept structured rather than pre-rendered into the status string the GraphQL
/// `ComplexObject` produces (`"dissolved:2019-04-02"`) — the profile header renders the date in
/// its own right, and a frontend splitting a colon-delimited string to get it back would be
/// parsing a display format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EntityStatusView {
    Active,
    Dissolved { on: String },
    InLiquidation { on: String },
}

/// The entity's status, with the provenance of that claim. Status is a fact like any other — an
/// entity shown as `Active` on the strength of an unattributable row is exactly the kind of
/// confident-looking claim PIV-03 exists to prevent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusFactView {
    pub value: EntityStatusView,
    pub provenance: Provenance,
}

/// One row of the "Identifiers & status" section: a labelled claim about *which* entity this is —
/// the Companies House number, the incorporation date, the registered name where it needs
/// attributing.
///
/// Deliberately a label/value pair rather than a fixed set of typed fields: the identifier set is
/// open (a company number today, a charity number or an OCOD proprietor id tomorrow), and a typed
/// field per identifier would mean a DTO change for every new one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentifierFactView {
    pub label: String,
    pub value: String,
    pub provenance: Provenance,
}

/// One officer, with the tenure dates PIV-02 asks for and PIV-04's worked example depends on
/// ("a director who left before a contract was awarded is visibly distinguished from one who was
/// serving"). Mirrors `queries.rs::DirectorRow`, which already carries all four fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OfficerRowView {
    pub name: String,
    #[serde(deserialize_with = "required_option")]
    pub role: Option<String>,
    /// ISO-8601 appointment date, `None` when the source row carries none.
    #[serde(deserialize_with = "required_option")]
    pub appointed: Option<String>,
    /// ISO-8601 resignation date; `None` means still serving as far as the data says — which is
    /// not the same as "confirmed still serving", a distinction the renderer must keep.
    #[serde(deserialize_with = "required_option")]
    pub resigned: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the page's "as at" filter made of it (PIV-04).
    ///
    /// Mandatory for the same reason `provenance` is: a temporal row that cannot say when it was
    /// in force has to say so inside the type ([`crate::temporal::EffectiveRange::Unrecorded`]),
    /// not by omitting a field. The range renders whether or not a filter is set — an unfiltered
    /// profile must not be the less informative screen.
    pub temporal: Temporal,
}

/// One address. Mirrors `queries.rs::AddressRow`.
///
/// `pivot` is `Some` since D-06 (plan 45-11 Task 1) put an `id` on `AddressRow` — the `LocatedAt`
/// destination node id, `location:…` or `LOC:…`, carried from the graph rather than derived from
/// the postcode. Landing it was a producer change and not a wire-format change, which is what the
/// field existing in advance was for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AddressRowView {
    #[serde(deserialize_with = "required_option")]
    pub postcode: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub address_line_1: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub locality: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// One contract award. Mirrors `queries.rs::ContractRow` minus `application_ref`, which belongs to
/// [`PlanningApplicationRowView`] — that today's single `ContractRow` carries *both* a
/// `tender_ref` and an `application_ref` is the clearest evidence it is two sections in one type
/// (D-03).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractRowView {
    pub name: String,
    #[serde(deserialize_with = "required_option")]
    pub value_gbp: Option<f64>,
    #[serde(deserialize_with = "required_option")]
    pub status: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub published: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub ward_code: Option<String>,
    /// Contracts Finder tender reference.
    #[serde(deserialize_with = "required_option")]
    pub tender_ref: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// One planning application — its own section (D-03), adjacent to contracts because they share the
/// `AwardedTo` edge type, never merged with them because PIV-02 lists them separately and because
/// a planning reference counted as a contract award inflates a figure the CLI report prints.
///
/// No `value_gbp`: a planning application has no award value, and a `None` money column on every
/// row would be a field the section exists without.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanningApplicationRowView {
    pub name: String,
    #[serde(deserialize_with = "required_option")]
    pub status: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub published: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub ward_code: Option<String>,
    /// Planning portal application reference.
    #[serde(deserialize_with = "required_option")]
    pub application_ref: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// One property record (CCOD / OCOD / PPD).
///
/// `EntityProfile::owns_property` is a `Vec<String>` today, so this row carries the one thing that
/// actually exists — the record's label — plus its pivot and its provenance. **Structured fields
/// (title number, tenure, price paid) are deliberately not declared here**: a column that is
/// `None` on every row for the whole phase is a stub that reads as missing data rather than as
/// absent structure. They land when a producer exists to fill them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PropertyRowView {
    pub label: String,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// One cross-registry identity link. Mirrors `queries.rs::OsintLink`.
///
/// **The link's confidence tier is not a field here.** It is the row's
/// [`Provenance::Attributed::tier`], which is where PIV-03 puts it — carrying it twice would give
/// a renderer two places to read one fact from, and eventually two answers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OsintLinkRowView {
    pub target_node_id: String,
    /// The OSINT system the link points into — `"electoral_commission"`, `"charity_commission"`,
    /// `"icij"`, `"opensanctions"`, …
    pub source_system: String,
    /// Composite confidence in [0.0, 1.0]. Distinct from the tier: the tier is the review state,
    /// this is the score that produced it.
    pub confidence: f64,
    /// How the link was established — `"exact_match"` and friends. Worth rendering because this
    /// project has twice purged large populations of name-only fuzzy links.
    pub resolution_type: String,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
}

/// One beneficial owner (PSC). Mirrors `queries.rs::BeneficialOwnerRow`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BeneficialOwnerRowView {
    pub name: String,
    pub natures_of_control: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// One disqualification record. Mirrors `queries.rs::DisqualificationRow`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisqualificationRowView {
    pub officer_name: String,
    #[serde(deserialize_with = "required_option")]
    pub case_number: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub disqualified_from: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub disqualified_until: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// One row of the three `Vec<String>`-shaped link families on the anchor — `trustee_of`,
/// `donates_to` and `offshore_links`. One row type rather than three identical ones; which family
/// a row belongs to is the section it arrives in, not a field repeating it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkedRecordRowView {
    pub label: String,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
    /// When this fact was in force, and what the "as at" filter made of it (PIV-04). See
    /// [`OfficerRowView::temporal`].
    pub temporal: Temporal,
}

/// OpenSanctions match status. Mirrors `queries.rs::SanctionsStatus`.
///
/// `on_list: false` is a real answer ("checked, no match"), which is why the whole struct being
/// `None` on the profile means something different: no OpenSanctions check is represented at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SanctionsStatusView {
    pub on_list: bool,
    #[serde(deserialize_with = "required_option")]
    pub match_name: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub match_score: Option<f64>,
    #[serde(deserialize_with = "required_option")]
    pub dataset: Option<String>,
    pub provenance: Provenance,
}

/// An entity reached from this one through a shared director or a shared registered address.
/// Mirrors `queries.rs::RelatedEntity`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelatedEntityRowView {
    pub name: String,
    #[serde(deserialize_with = "required_option")]
    pub ch_number: Option<String>,
    pub status: EntityStatusView,
    /// What is shared: `kind` is `"director"` or `"address"`, `value` the officer name or postcode.
    pub shared_connection: SharedConnectionView,
    #[serde(deserialize_with = "required_option")]
    pub pivot: Option<EntityRef>,
    pub provenance: Provenance,
}

/// The shared-connection descriptor on a [`RelatedEntityRowView`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedConnectionView {
    pub kind: String,
    pub value: String,
}

/// This entity's presence in one LPA. Mirrors `queries.rs::LpaPresence`.
///
/// **Carries no provenance, deliberately.** It is a count spanning two datasets (Contracts Finder
/// and the planning portals); naming one of them as *the* source of the combined figure would be
/// the placeholder attribution D-04 forbids. The rows it counts carry their own provenance in
/// their own sections, which is where an analyst checks it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LpaPresenceView {
    pub lpa_code: String,
    #[serde(deserialize_with = "required_option")]
    pub lpa_name: Option<String>,
    pub contract_count: u64,
    pub planning_application_count: u64,
}

/// One probe finding whose evidence names this entity (D-05, plan 45-05 builds the index).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FindingRowView {
    /// The probe that produced the finding.
    pub probe: String,
    /// The finding's headline, as the probe worded it.
    pub summary: String,
    /// The probe's verdict, reusing this crate's existing mirror so a finding on the profile and
    /// the same finding on the Analysis page cannot render as different verdicts.
    pub verdict: VerdictView,
    /// Which `RecordRef` kind named this entity, from `RecordKind::name`. `"Entity"`,
    /// `"CompanyNumber"` or `"IdentityLink"` in practice — those are the only kinds a probe
    /// constructs; `RecordKind`'s two node-class variants (`"Contract"`, `"Location"`) are
    /// emitted by the profile mapper's pivots, not by probe evidence. An analyst asking "why is
    /// this finding on this profile?" is asking this question.
    pub matched_via: String,
    pub provenance: Provenance,
}

/// One motif alert. Mirrors `queries.rs::AlertRecord`, minus `result_json` (a payload, not a row).
///
/// The withdrawal fields are carried because detection never retracts what it stops finding: an
/// alert invalidated by a data correction survives its own correction, and marking rather than
/// deleting keeps that as evidence. A profile that showed a withdrawn alert as live would be
/// `T-23-FalseAccusation` with a nicer stylesheet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertRowView {
    pub id: String,
    pub motif_type: String,
    pub score: f64,
    /// ISO-8601 detection time.
    pub detected_at: String,
    #[serde(deserialize_with = "required_option")]
    pub ward_code: Option<String>,
    /// `Some` when an operator retracted this alert; `None` means live.
    #[serde(deserialize_with = "required_option")]
    pub withdrawn_at: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub withdrawn_reason: Option<String>,
    pub provenance: Provenance,
}

/// One OSINT-sourced motif alert. Mirrors `queries.rs::OsintAlertRow`, which carries only the
/// motif type and a severity band — thinner than [`AlertRowView`] because the underlying row is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OsintAlertRowView {
    pub motif_type: String,
    /// `"CRITICAL"` / `"HIGH"` / `"MEDIUM"`, derived server-side from the risk score.
    pub severity: String,
    pub provenance: Provenance,
}

/// Section 8, "Findings & alerts" — and the section whose *empty* state is the most dangerous
/// thing on this screen.
///
/// The plan 44-21 summary measured it: twelve of thirteen probes emit no entity id at all, their
/// evidence interpolating counts and adapter names instead. So for most entities this section is
/// empty, and an unqualified "No findings" would read as "this entity is clean" when the truth is
/// "no probe was capable of looking".
///
/// [`Self::probes_total`] and [`Self::probes_unable_to_name_an_entity`] are **mandatory, non-`Option`
/// counts** for that reason: a renderer cannot show this section's empty state without also having
/// the numbers that qualify it, which is the phase 45 UI spec's "{k} of {m} probes report only aggregate
/// counts and cannot name an entity at all" enforced at the type level rather than in copy review.
/// The three answers the findings half of this section can give — the wire mirror of
/// `crates/core::persistence::queries::FindingsOutcome`.
///
/// **Added by plan 45-08 on plan 45-05's explicit instruction**, which is worth reading in full
/// because it names the defect this field prevents: *"carry `outcome` and `coverage`, not just
/// `rows`. A DTO with only `rows` reintroduces exactly the bug this plan exists to prevent, and it
/// will look correct in every test that has data."* 45-01 shaped [`FindingsSectionView`] before
/// 45-05 built the index, so the two counts below could say how many probes *can* name an entity
/// but not whether any of them actually looked at this one.
///
/// [`Self::NotNamed`] and [`Self::NotSearched`] both render as `rows: []` and mean opposite
/// things: the first is "probes that can name an entity ran and none named this one", the second
/// is "no probe capable of naming an entity contributed a finding at all, so this empty list is
/// not evidence about the entity". The phase 45 UI spec's three-way empty-state rule cannot be
/// rendered without this distinction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingsOutcomeView {
    /// At least one searchable producer named this entity; `findings.rows` is non-empty.
    Named,
    /// Searchable probes contributed findings and none of them named this entity. The only
    /// outcome in which an empty list means "no findings".
    NotNamed,
    /// No searchable probe contributed a finding at all. An empty list here says nothing about
    /// the entity.
    NotSearched,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FindingsSectionView {
    pub findings: Page<FindingRowView>,
    pub alerts: Page<AlertRowView>,
    pub osint_alerts: Page<OsintAlertRowView>,
    /// Which of the three answers the findings page is (see [`FindingsOutcomeView`]). Mandatory
    /// and non-`Option`: an empty findings page whose outcome key is missing is the exact
    /// ambiguity this section is most dangerous for.
    pub outcome: FindingsOutcomeView,
    /// How many probes ran in the analysis this profile is scoped to (`m`).
    pub probes_total: u32,
    /// How many of those emit aggregate-only evidence and therefore cannot name any entity (`k`).
    /// A number this large next to an empty list is the honest answer; a blank is not.
    pub probes_unable_to_name_an_entity: u32,
}

/// Everything the profile screen renders about one entity (PIV-02), and the type the canvas
/// inspection panel's `Compact` density renders from too (D-11) — there is one profile renderer,
/// so there is one profile DTO.
///
/// `id`, `name` and `kind` name the *subject* of the page rather than making a claim about it;
/// every claim lives in a section and carries its provenance. A registered-name claim that needs
/// attributing is emitted as an [`IdentifierFactView`] like any other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityProfileDto {
    /// The graph id, e.g. `"director:jane-smith"` or `"GB-COH:99000001"`.
    pub id: String,
    pub name: String,
    /// The subject's node-class name, as on [`EntityRef`] — the same vocabulary the
    /// disambiguation list's `name · kind · id` rows use.
    ///
    /// Filled from the label index, i.e. `crates/workbench`'s `node_kind_name`, so `"Entity"`,
    /// `"Officer"`, `"Ward"` and the rest all occur here. (45-01 described this as "the
    /// `RecordKind`-shaped kind name"; it never was — see this module's doc comment on the two
    /// vocabularies and how D-06 reconciled their spellings.)
    pub kind: String,
    pub status: StatusFactView,
    pub identifiers: Vec<IdentifierFactView>,
    pub officers: Page<OfficerRowView>,
    pub addresses: Page<AddressRowView>,
    pub contracts: Page<ContractRowView>,
    pub planning_applications: Page<PlanningApplicationRowView>,
    pub property: Page<PropertyRowView>,
    pub osint_links: Page<OsintLinkRowView>,
    pub beneficial_owners: Page<BeneficialOwnerRowView>,
    pub disqualifications: Page<DisqualificationRowView>,
    pub trustee_of: Page<LinkedRecordRowView>,
    pub donates_to: Page<LinkedRecordRowView>,
    pub offshore_links: Page<LinkedRecordRowView>,
    /// `None` means no OpenSanctions check is represented for this entity — not "no match", which
    /// is `Some(SanctionsStatusView { on_list: false, .. })`. Always a present key.
    #[serde(deserialize_with = "required_option")]
    pub sanctions: Option<SanctionsStatusView>,
    /// Entities sharing a director or a registered address — the neighbour pivot surface.
    pub related: Page<RelatedEntityRowView>,
    pub lpa_presence: Vec<LpaPresenceView>,
    pub findings: FindingsSectionView,
    /// The ISO-8601 date this profile was evaluated "as at", or `None` for an unfiltered profile
    /// (PIV-04). Always a present key: a reader who cannot tell a filtered profile from an
    /// unfiltered one will misread the page, which is why the phase 45 UI spec gives the as-at banner
    /// the only non-verdict use of `--warning` on this screen.
    #[serde(deserialize_with = "required_option")]
    pub as_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::LinkTier;
    use crate::temporal::{AsAtStatus, EffectiveRange};
    use crate::verdict_view::{CoverageView, PopulationSourceView};
    use serde_json::json;
    use serde_json::Value;

    fn attributed(source: &str) -> Provenance {
        Provenance::Attributed {
            source_dataset: source.to_string(),
            observed_at: "2026-07-31T00:00:00Z".to_string(),
            tier: None,
        }
    }

    fn linked(source: &str, tier: LinkTier) -> Provenance {
        Provenance::Attributed {
            source_dataset: source.to_string(),
            observed_at: "2026-07-31T00:00:00Z".to_string(),
            tier: Some(tier),
        }
    }

    fn dated(from: Option<&str>, to: Option<&str>, basis: &str) -> Temporal {
        Temporal::unfiltered(EffectiveRange::Recorded {
            from: from.map(str::to_string),
            to: to.map(str::to_string),
            basis: basis.to_string(),
        })
    }

    fn undated(basis: &str) -> Temporal {
        Temporal::unfiltered(EffectiveRange::Unrecorded {
            basis: basis.to_string(),
        })
    }

    fn pivot(id: &str, name: &str, kind: &str) -> Option<EntityRef> {
        Some(EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: kind.to_string(),
        })
    }

    fn page<T>(rows: Vec<T>, total: u64, next: Option<&str>) -> Page<T> {
        Page {
            rows,
            next_cursor: next.map(str::to_string),
            total,
        }
    }

    /// A profile with at least one row in every section, so the sweeps below have something to
    /// sweep. Ward codes use the synthetic `X05…` prefix that
    /// `scripts/check-no-analysis-literals.sh` requires of workbench fixtures.
    fn sample() -> EntityProfileDto {
        EntityProfileDto {
            id: "GB-COH:99000001".to_string(),
            name: "Example Holdings Limited".to_string(),
            kind: "Entity".to_string(),
            status: StatusFactView {
                value: EntityStatusView::Dissolved {
                    on: "2019-04-02".to_string(),
                },
                provenance: attributed("CH"),
            },
            identifiers: vec![IdentifierFactView {
                label: "Companies House number".to_string(),
                value: "99000001".to_string(),
                provenance: attributed("CH"),
            }],
            officers: page(
                vec![OfficerRowView {
                    name: "Jane Smith".to_string(),
                    role: Some("director".to_string()),
                    appointed: Some("2015-01-04".to_string()),
                    resigned: Some("2021-09-30".to_string()),
                    pivot: pivot("director:jane-smith", "Jane Smith", "Entity"),
                    provenance: attributed("CH"),
                    temporal: dated(
                        Some("2015-01-04"),
                        Some("2021-09-30"),
                        "officer.appointed_on / officer.resigned_on",
                    ),
                }],
                2_400,
                Some("officers-page-2"),
            ),
            addresses: page(
                vec![AddressRowView {
                    postcode: Some("XX1 1XX".to_string()),
                    address_line_1: Some("1 Example Street".to_string()),
                    locality: None,
                    pivot: None,
                    provenance: attributed("CH"),
                    temporal: undated(
                        "located_at.meta.effective_from / located_at.meta.effective_to",
                    ),
                }],
                1,
                None,
            ),
            contracts: page(
                vec![ContractRowView {
                    name: "Highway resurfacing".to_string(),
                    value_gbp: Some(125_000.0),
                    status: Some("awarded".to_string()),
                    published: Some("2024-06-11".to_string()),
                    ward_code: Some("X05999001".to_string()),
                    tender_ref: Some("tender-0001".to_string()),
                    pivot: pivot("contract:tender-0001", "Highway resurfacing", "Entity"),
                    provenance: attributed("CF"),
                    temporal: dated(Some("2024-06-11"), None, "contract.date_published"),
                }],
                12,
                None,
            ),
            planning_applications: page(
                vec![PlanningApplicationRowView {
                    name: "Change of use, 1 Example Street".to_string(),
                    status: Some("approved".to_string()),
                    published: Some("2023-02-17".to_string()),
                    ward_code: Some("X05999001".to_string()),
                    application_ref: Some("48C93".to_string()),
                    pivot: pivot("planning:48C93", "48C93", "Entity"),
                    provenance: attributed("planning"),
                    temporal: dated(Some("2023-02-17"), None, "contract.date_published"),
                }],
                3,
                None,
            ),
            property: page(
                vec![PropertyRowView {
                    label: "Title XX123456".to_string(),
                    pivot: None,
                    provenance: attributed("CCOD"),
                    temporal: dated(
                        Some("2022-07-06T00:00:00Z"),
                        None,
                        "owns_property.meta.effective_from / owns_property.meta.effective_to",
                    ),
                }],
                1,
                None,
            ),
            osint_links: page(
                vec![OsintLinkRowView {
                    target_node_id: "PA:example-party".to_string(),
                    source_system: "electoral_commission".to_string(),
                    confidence: 0.94,
                    resolution_type: "exact_match".to_string(),
                    pivot: pivot("PA:example-party", "Example Party", "IdentityLink"),
                    provenance: linked("EC", LinkTier::Confirmed),
                }],
                1,
                None,
            ),
            beneficial_owners: page(
                vec![BeneficialOwnerRowView {
                    name: "Jane Smith".to_string(),
                    natures_of_control: vec!["ownership-of-shares-75-to-100-percent".to_string()],
                    pivot: pivot("director:jane-smith", "Jane Smith", "Entity"),
                    provenance: linked("PSC", LinkTier::AutoLinked),
                    temporal: dated(
                        Some("2016-04-06T00:00:00Z"),
                        None,
                        "controls.meta.effective_from / controls.meta.effective_to",
                    ),
                }],
                1,
                None,
            ),
            disqualifications: page(
                vec![DisqualificationRowView {
                    officer_name: "John Doe".to_string(),
                    case_number: None,
                    disqualified_from: Some("2020-05-01".to_string()),
                    disqualified_until: Some("2028-05-01".to_string()),
                    pivot: None,
                    provenance: linked("CH", LinkTier::Confirmed),
                    temporal: dated(
                        Some("2020-05-01"),
                        Some("2028-05-01"),
                        "disqualification.disqualified_from / disqualification.disqualified_until",
                    ),
                }],
                1,
                None,
            ),
            trustee_of: page(
                vec![LinkedRecordRowView {
                    label: "Example Charitable Trust".to_string(),
                    pivot: None,
                    provenance: linked("CC", LinkTier::Confirmed),
                    temporal: undated(
                        "trustee_of.meta.effective_from / trustee_of.meta.effective_to",
                    ),
                }],
                1,
                None,
            ),
            donates_to: page(
                vec![LinkedRecordRowView {
                    label: "Example Party".to_string(),
                    pivot: None,
                    provenance: linked("EC", LinkTier::Confirmed),
                    temporal: undated(
                        "donates_to.meta.effective_from / donates_to.meta.effective_to",
                    ),
                }],
                1,
                None,
            ),
            offshore_links: page(
                vec![LinkedRecordRowView {
                    label: "Example Offshore Holdings".to_string(),
                    pivot: None,
                    provenance: Provenance::Unattributed {
                        reason: "no source event found for this row".to_string(),
                    },
                    temporal: undated(
                        "offshore_link.meta.effective_from / offshore_link.meta.effective_to",
                    ),
                }],
                1,
                None,
            ),
            sanctions: Some(SanctionsStatusView {
                on_list: false,
                match_name: None,
                match_score: None,
                dataset: None,
                provenance: attributed("OpenSanctions"),
            }),
            related: page(
                vec![RelatedEntityRowView {
                    name: "Example Trading Limited".to_string(),
                    ch_number: Some("04704172".to_string()),
                    status: EntityStatusView::Active,
                    shared_connection: SharedConnectionView {
                        kind: "director".to_string(),
                        value: "Jane Smith".to_string(),
                    },
                    pivot: pivot("GB-COH:04704172", "Example Trading Limited", "Entity"),
                    provenance: attributed("CH"),
                }],
                48,
                Some("related-page-2"),
            ),
            lpa_presence: vec![LpaPresenceView {
                lpa_code: "X06999001".to_string(),
                lpa_name: None,
                contract_count: 12,
                planning_application_count: 3,
            }],
            as_at: None,
            findings: FindingsSectionView {
                findings: page(
                    vec![FindingRowView {
                        probe: "ward-concentration".to_string(),
                        summary: "One supplier holds 61% of ward spend".to_string(),
                        verdict: VerdictView::Hit {
                            coverage: CoverageView {
                                population: 100,
                                capable: 90,
                                excluded: 10,
                            },
                            matches: 3,
                            population_source: PopulationSourceView::OracleCorroborated {
                                oracle_count: 90,
                            },
                        },
                        matched_via: "Entity".to_string(),
                        provenance: attributed("probe"),
                    }],
                    1,
                    None,
                ),
                alerts: page(
                    vec![AlertRowView {
                        id: "alert:0001".to_string(),
                        motif_type: "PhoenixLoop".to_string(),
                        score: 0.85,
                        detected_at: "2026-08-11T09:00:00Z".to_string(),
                        ward_code: Some("X05999001".to_string()),
                        withdrawn_at: None,
                        withdrawn_reason: None,
                        provenance: attributed("detector"),
                    }],
                    1,
                    None,
                ),
                osint_alerts: page(
                    vec![OsintAlertRowView {
                        motif_type: "DonationCorrelation".to_string(),
                        severity: "HIGH".to_string(),
                        provenance: attributed("EC"),
                    }],
                    1,
                    None,
                ),
                outcome: FindingsOutcomeView::Named,
                probes_total: 13,
                probes_unable_to_name_an_entity: 12,
            },
        }
    }

    /// Every top-level key whose value has the `rows`/`next_cursor`/`total` shape of a
    /// [`Page`], paired with its rows — including the three pages nested inside the findings
    /// section.
    fn pages(profile_json: &Value) -> Vec<(String, Vec<Value>)> {
        fn collect(prefix: &str, object: &Value, out: &mut Vec<(String, Vec<Value>)>) {
            for (key, value) in object.as_object().expect("an object") {
                let Some(fields) = value.as_object() else {
                    continue;
                };
                let is_page = fields.contains_key("rows")
                    && fields.contains_key("next_cursor")
                    && fields.contains_key("total");
                if is_page {
                    let rows = fields["rows"].as_array().expect("rows is an array").clone();
                    out.push((format!("{prefix}{key}"), rows));
                } else {
                    collect(&format!("{prefix}{key}."), value, out);
                }
            }
        }
        let mut out = Vec::new();
        collect("", profile_json, &mut out);
        out
    }

    /// The whole profile survives a JSON round trip — every section, every row type, every
    /// `required_option` key.
    #[test]
    fn a_full_profile_round_trips() {
        let profile = sample();
        let json = serde_json::to_value(&profile).expect("EntityProfileDto must serialize");
        let back: EntityProfileDto =
            serde_json::from_value(json).expect("EntityProfileDto must deserialize");
        assert_eq!(back, profile);
    }

    /// PIV-03 at the wire: every row in every paged section carries a `provenance` key.
    ///
    /// The section list is derived from the serialized profile rather than written out here, so a
    /// section added later is swept automatically — the sample cannot be constructed without it
    /// (no field has a default), and the sweep finds whatever the sample contains.
    ///
    /// PROVE THIS CAN FAIL: run with `provenance` removed from `PropertyRowView`, the sweep
    /// reports `property` as the offending section and the test fails. That mutation was performed
    /// before this test was committed.
    #[test]
    fn every_row_in_every_section_carries_its_provenance() {
        let json = serde_json::to_value(sample()).expect("serialize");
        let sections = pages(&json);
        assert!(
            sections.len() >= 15,
            "expected the profile to expose at least 15 paged sections, found {}: {:?}",
            sections.len(),
            sections.iter().map(|(name, _)| name).collect::<Vec<_>>()
        );

        let mut rows_checked = 0;
        for (section, rows) in &sections {
            assert!(
                !rows.is_empty(),
                "section `{section}` has no sample row, so this sweep would pass vacuously over it"
            );
            for row in rows {
                assert!(
                    row.get("provenance").is_some(),
                    "a row in section `{section}` carries no provenance: {row}"
                );
                rows_checked += 1;
            }
        }
        assert_eq!(
            rows_checked,
            sections.len(),
            "expected one sample row per section"
        );
    }

    /// PIV-04 at the wire: every row in a **temporal** section carries its effective range, and a
    /// producer that has not applied a filter says `NotFiltered` rather than claiming `InForce`.
    ///
    /// The temporal sections are named explicitly rather than swept, because the claim is not
    /// "every section" — `osint_links`, `findings` and the two alert pages are assertions made at
    /// a moment rather than facts in force over an interval, and giving them a range would be the
    /// placeholder this phase forbids in the neighbouring dimension.
    ///
    /// PROVE THIS CAN FAIL: run with `temporal` removed from `PropertyRowView`, or with the
    /// property sample row's `temporal` replaced by `Temporal::unfiltered(...)` carrying
    /// `AsAtStatus::InForce`; the first fails to compile the sweep's lookup and the second fails
    /// the status assertion naming `property`.
    #[test]
    fn every_row_in_every_temporal_section_carries_its_effective_range() {
        const TEMPORAL_SECTIONS: [&str; 8] = [
            "officers",
            "addresses",
            "contracts",
            "planning_applications",
            "property",
            "beneficial_owners",
            "disqualifications",
            "trustee_of",
        ];
        let json = serde_json::to_value(sample()).expect("serialize");
        let sections: std::collections::BTreeMap<String, Vec<Value>> =
            pages(&json).into_iter().collect();

        let mut rows_checked = 0;
        for name in TEMPORAL_SECTIONS {
            let rows = sections
                .get(name)
                .unwrap_or_else(|| panic!("section `{name}` is missing from the profile"));
            assert!(
                !rows.is_empty(),
                "NON-VACUITY: section `{name}` has no sample row, so this sweep would pass                  vacuously over it"
            );
            for row in rows {
                let temporal = row
                    .get("temporal")
                    .unwrap_or_else(|| panic!("a row in `{name}` carries no temporal: {row}"));
                assert!(
                    temporal["range"]["range"].is_string(),
                    "a row in `{name}` carries a temporal with no range tag: {temporal}"
                );
                assert!(
                    temporal["range"]["basis"].is_string(),
                    "a row in `{name}` carries a range that does not name the field it was read \
                     from: {temporal}"
                );
                assert_eq!(
                    temporal["status"]["as_at"],
                    json!("not_filtered"),
                    "an unfiltered profile must not claim a row was in force: `{name}`"
                );
                rows_checked += 1;
            }
        }
        assert_eq!(rows_checked, TEMPORAL_SECTIONS.len());
    }

    /// The fixture exercises BOTH arms of the range vocabulary. A sweep over rows that were all
    /// `Recorded` — or all `Unrecorded` — would pass against a producer that emitted one constant.
    #[test]
    fn the_sample_profile_carries_both_a_recorded_and_an_unrecorded_range() {
        let json = serde_json::to_value(sample()).expect("serialize");
        let mut recorded = 0;
        let mut unrecorded = 0;
        for (_, rows) in pages(&json) {
            for row in rows {
                match row.get("temporal").map(|t| t["range"]["range"].clone()) {
                    Some(v) if v == json!("recorded") => recorded += 1,
                    Some(v) if v == json!("unrecorded") => unrecorded += 1,
                    _ => {}
                }
            }
        }
        assert!(
            recorded > 0 && unrecorded > 0,
            "NON-VACUITY: the fixture must show both a dated and an undated row              (recorded={recorded}, unrecorded={unrecorded}) — on the live corpus the undated case              is the overwhelming majority and a fixture that omitted it would guard nothing"
        );
    }

    /// A temporal row without its range does not deserialize — the field is mandatory on the wire,
    /// not merely present in the Rust type.
    #[test]
    fn a_temporal_row_without_its_range_fails_to_deserialize() {
        let json = serde_json::to_value(sample()).expect("serialize");
        let mut row = json["officers"]["rows"][0].clone();
        serde_json::from_value::<OfficerRowView>(row.clone())
            .expect("the unmodified officer row deserializes");
        row.as_object_mut().unwrap().remove("temporal");
        assert!(
            serde_json::from_value::<OfficerRowView>(row).is_err(),
            "an omitted `temporal` must be an error, not a silently-defaulted range"
        );
    }

    /// PIV-04's worked example, at the wire: the officer resigned before the contract was
    /// published, and both dates are on the page rather than left for the reader to look up.
    #[test]
    fn the_worked_example_is_expressible_on_the_wire() {
        let profile = sample();
        let officer = &profile.officers.rows[0];
        let contract = &profile.contracts.rows[0];
        let EffectiveRange::Recorded {
            to: Some(resigned), ..
        } = &officer.temporal.range
        else {
            panic!("the sample officer must record a resignation date");
        };
        let EffectiveRange::Recorded {
            from: Some(published),
            ..
        } = &contract.temporal.range
        else {
            panic!("the sample contract must record a publication date");
        };
        assert!(
            resigned.as_str() < published.as_str(),
            "the fixture must show a director who left BEFORE the award ({resigned} vs              {published}) — that comparison is the whole of PIV-04"
        );
        assert_eq!(officer.temporal.status, AsAtStatus::NotFiltered);
    }

    /// The non-row facts carry provenance too: status, and each identifier.
    #[test]
    fn the_status_and_identifier_facts_carry_provenance() {
        let json = serde_json::to_value(sample()).expect("serialize");
        assert!(json["status"]["provenance"].is_object());
        let identifiers = json["identifiers"].as_array().expect("identifiers array");
        assert!(!identifiers.is_empty(), "no identifier facts to check");
        for identifier in identifiers {
            assert!(
                identifier.get("provenance").is_some(),
                "an identifier fact carries no provenance: {identifier}"
            );
        }
    }

    /// A row without provenance does not deserialize — the field is mandatory on the wire, not
    /// merely present in the Rust type.
    #[test]
    fn a_row_without_provenance_fails_to_deserialize() {
        let mut json = serde_json::to_value(sample()).expect("serialize");
        json["officers"]["rows"][0]
            .as_object_mut()
            .expect("row object")
            .remove("provenance");
        assert!(
            serde_json::from_value::<EntityProfileDto>(json).is_err(),
            "an officer row without provenance must not deserialize"
        );
    }

    /// ENR-11 / D-13: sub-candidate links and the `pending_review` tier never appear anywhere in
    /// the serialized profile, at any depth.
    ///
    /// PROVE THIS CAN FAIL: adding a `sub_candidate_links` field to `EntityProfileDto` (or a
    /// `pending_review` tier to `LinkTier`, reachable from any row's provenance) turns this red.
    /// The `LinkTier` half of that mutation was performed in plan 45-01 task 1.
    #[test]
    fn sub_candidate_links_and_pending_review_never_cross_this_boundary() {
        let json = serde_json::to_string(&sample()).expect("serialize");
        for forbidden in ["sub_candidate", "subCandidate", "pending_review"] {
            assert!(
                !json.contains(forbidden),
                "`{forbidden}` must never appear on the wasm boundary: {json}"
            );
        }
    }

    /// D-03 at the wire: contracts and planning applications are two sections with two totals, so
    /// a planning reference can never be counted as a contract award.
    #[test]
    fn contracts_and_planning_applications_are_separate_sections() {
        let json = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(json["contracts"]["total"], 12);
        assert_eq!(json["planning_applications"]["total"], 3);
        assert!(
            json["contracts"]["rows"][0]
                .get("application_ref")
                .is_none(),
            "a contract row must not carry a planning application reference"
        );
        assert!(
            json["planning_applications"]["rows"][0]
                .get("tender_ref")
                .is_none(),
            "a planning row must not carry a tender reference"
        );
    }

    /// The findings section cannot be rendered without the counts that qualify its empty state —
    /// D-05's "twelve of thirteen probes cannot name an entity" is carried, not assumed.
    #[test]
    fn the_findings_section_cannot_omit_its_probe_coverage_counts() {
        let json = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(json["findings"]["probes_total"], 13);
        assert_eq!(json["findings"]["probes_unable_to_name_an_entity"], 12);

        for key in ["probes_total", "probes_unable_to_name_an_entity"] {
            let mut stripped = serde_json::to_value(sample()).expect("serialize");
            stripped["findings"]
                .as_object_mut()
                .expect("findings object")
                .remove(key);
            assert!(
                serde_json::from_value::<EntityProfileDto>(stripped).is_err(),
                "a findings section without `{key}` must not deserialize"
            );
        }
    }

    /// A paged section states how many rows exist beyond the window it returned — the truth this
    /// plan's paging envelope exists for, checked on a real section rather than on `Page<String>`.
    #[test]
    fn a_section_reports_rows_beyond_its_page() {
        let profile = sample();
        assert_eq!(profile.officers.rows.len(), 1);
        assert_eq!(profile.officers.total, 2_400);
        assert_eq!(
            profile.officers.next_cursor.as_deref(),
            Some("officers-page-2")
        );
    }

    /// `sanctions` distinguishes "no OpenSanctions check represented" (`null`) from "checked, no
    /// match" (`on_list: false`), and the key is always present.
    #[test]
    fn an_absent_sanctions_check_is_a_present_null() {
        let mut profile = sample();
        profile.sanctions = None;
        let json = serde_json::to_string(&profile).expect("serialize");
        assert!(
            json.contains("\"sanctions\":null"),
            "the sanctions key must be present and null, not omitted"
        );

        let mut stripped = serde_json::to_value(sample()).expect("serialize");
        stripped
            .as_object_mut()
            .expect("profile object")
            .remove("sanctions");
        assert!(
            serde_json::from_value::<EntityProfileDto>(stripped).is_err(),
            "an omitted `sanctions` key must be an error, not a silent None"
        );
    }

    /// The dissolution date stays structured across the boundary — the frontend never has to split
    /// GraphQL's `"dissolved:2019-04-02"` display string to render a date.
    #[test]
    fn entity_status_keeps_its_date_structured() {
        let json = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(json["status"]["value"]["status"], "dissolved");
        assert_eq!(json["status"]["value"]["on"], "2019-04-02");
    }
}
