//! `EntityProfile` → `EntityProfileDto`: a **move, not a construction** (PIV-06 / 45-CONTEXT D-01).
//!
//! # The rule this module exists to keep
//!
//! Every value below is carried across from `crates/core::persistence::queries::EntityProfile`.
//! Nothing here queries, infers or synthesises a fact: no SurrealQL, no `Provenance` built on the
//! spot, no date invented. 45-04 put it in one line — *"do not synthesise a provenance anywhere in
//! the handler"* — and the same applies to every other field. `Provenance` and `Temporal` are
//! already `workbench-dto` types on the core rows (45-04, 45-06), so those two travel by `clone()`
//! rather than through a conversion that could drift.
//!
//! # Windowing happens here, and only here
//!
//! 45-02's settled rule: *"windowing belongs at the wire boundary (`workbench-dto`'s `Page<T>`),
//! not in `crates/core::persistence`"*. The read path returns every row it has; [`section_page`]
//! is the one place a window is taken, and it emits `total` from the **unwindowed** vector, so a
//! page can never be mistaken for the whole section.
//!
//! # What a cursor is
//!
//! `"{section}@{offset}"`. It is **opaque** by contract (`Page::next_cursor`'s doc comment): the
//! browser passes it back and must never parse, increment or construct one. It is readable rather
//! than encoded because an opaque token that is also legible is easier to debug and no less
//! opaque — the contract is the client's behaviour, not the bytes. The section name is inside it
//! so a cursor handed to the wrong section is rejected rather than silently applied to a different
//! list.
//!
//! # Pivots: what this mapping can and cannot supply
//!
//! 45-01 declared `pivot: Option<EntityRef>` on every row type. 45-08 measured that the read path
//! could fill two of them and pinned the number. Plan 45-11's Task 1 (D-06) put an `id` on
//! `ContractRow`, `PlanningApplicationRow` and `AddressRow` — carried from the `AwardedTo` /
//! `LocatedAt` destination, never minted — so **five** are filled now:
//!
//! * `OsintLinkRowView` — `OsintLink::target_node_id` is an id.
//! * `RelatedEntityRowView` — `RelatedEntity::ch_number` reconstructs `GB-COH:{number}`, the same
//!   id `queries.rs` builds when matching a `RecordKind::CompanyNumber` ref against a profile.
//!   Only for a related **company**; a related director slug still has no id on the row.
//! * `ContractRowView` / `PlanningApplicationRowView` — `RecordKind::Contract`, the `AwardedTo`
//!   destination.
//! * `AddressRowView` — `RecordKind::Location`, the `LocatedAt` destination.
//!
//! Still `None`, and still for the reason 45-08 gave — the row is built from a node's *weight* and
//! keeps no id, and inventing one from a display label would be a link that lands on the wrong
//! entity or on nothing: `OfficerRowView`, `PropertyRowView`, `LinkedRecordRowView` (trustee-of,
//! donates-to, offshore links), `BeneficialOwnerRowView`, `DisqualificationRowView`, and
//! `RelatedEntityRowView` for a director.
//!
//! ## What a pivot id is not
//!
//! **An id being real does not make the target renderable.** `query_entity_by_id` resolves
//! `Entity` and `OffshoreEntity` node weights only, so `GET /api/entity/{id}` 404s for every
//! `location:`, `LOC:`, `CF:` and `planning:` id this mapping now emits — on the live corpus zero
//! of those ids has an `Entity` node (measured 2026-09-15). The ids are correct and the
//! destination page does not exist yet. 45-11's Task 2 must not render these as links until a
//! subject lookup can open a `Location`/`Contract` node; the gap is pinned by
//! `db_a_contract_or_address_pivot_id_names_a_node_the_profile_route_cannot_open_yet` in
//! `crates/core/tests/profile_pivot_ids.rs`, which fails the day it closes.
//!
//! The width is pinned by `pivot_coverage_is_what_the_read_path_can_supply` in
//! `crates/workbench/tests/entity_api.rs` rather than left to be discovered by the renderer.
//! **When a later plan puts ids on the remaining row types, that test fails** — update this
//! mapping in the same change, per 45-03's keep-the-field-to-keep-the-test rule.

use urban_integrity_core::persistence::queries::{
    AddressRow, AlertRecord, BeneficialOwnerRow, ContractRow, DirectorRow, DisqualificationRow,
    EntityProfile, FindingOrigin, FindingRef, FindingsByEntity, FindingsOutcome, LinkedRecordRow,
    LpaPresence, OsintAlertRow, OsintLink, PlanningApplicationRow, PropertyRow, RelatedEntity,
    SanctionsStatus,
};
use urban_integrity_probe::report::ReportEntry;
use urban_integrity_schema::types::node::EntityStatus;
use urban_integrity_schema::types::verdict::RecordKind;
use urban_integrity_workbench_dto::caps::{CEILING_PROFILE_PAGE_SIZE, DEFAULT_PROFILE_PAGE_SIZE};
use urban_integrity_workbench_dto::{
    AddressRowView, AlertRowView, BeneficialOwnerRowView, ContractRowView, DisqualificationRowView,
    EntityProfileDto, EntityRef, EntityStatusView, FindingRowView, FindingsOutcomeView,
    FindingsSectionView, IdentifierFactView, LinkedRecordRowView, LpaPresenceView, OfficerRowView,
    OsintAlertRowView, OsintLinkRowView, Page, PlanningApplicationRowView, PropertyRowView,
    RelatedEntityRowView, SanctionsStatusView, SharedConnectionView, StatusFactView,
};

use crate::graph::label_index::LabelIndex;

/// Every section name a cursor may name. A request naming anything else is rejected, so a typo in
/// a client cannot silently page a section nobody asked for.
pub const SECTIONS: &[&str] = &[
    "officers",
    "addresses",
    "contracts",
    "planning_applications",
    "property",
    "osint_links",
    "beneficial_owners",
    "disqualifications",
    "trustee_of",
    "donates_to",
    "offshore_links",
    "related",
    "findings",
    "alerts",
    "osint_alerts",
];

/// Which window of which section the caller asked for.
///
/// `section`/`offset` are `None`/`0` for a first load: every section returns its first window. A
/// **Show more** press names one section and one offset, and only that section moves — the rest of
/// the document is re-sent at its first window, so the response is always a complete, coherent
/// profile rather than a fragment the browser has to splice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionWindow {
    pub section: Option<String>,
    pub offset: usize,
    pub page_size: usize,
}

impl Default for SectionWindow {
    fn default() -> Self {
        Self {
            section: None,
            offset: 0,
            page_size: DEFAULT_PROFILE_PAGE_SIZE as usize,
        }
    }
}

/// Why a request's paging parameters could not be used. Rejected rather than repaired: a cursor
/// for the wrong section silently applied to another one would page a list the analyst is not
/// looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    UnknownSection(String),
    MalformedCursor(String),
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::UnknownSection(s) => {
                write!(
                    f,
                    "unknown profile section {s:?} — expected one of {SECTIONS:?}"
                )
            }
            WindowError::MalformedCursor(c) => write!(
                f,
                "malformed cursor {c:?} — pass back a `next_cursor` exactly as it was received"
            ),
        }
    }
}

/// Build the window a request asked for.
///
/// `page_size` is clamped to `workbench-dto::caps` (D-13) rather than trusted: the ceiling is the
/// server's, and a browser asking for 10,000 rows gets [`CEILING_PROFILE_PAGE_SIZE`]. A zero is
/// raised to 1 for the reason `caps`'s own build-time assertion names — a zero-row page never
/// terminates however many times **Show more** is pressed.
pub fn window_from_request(
    section: Option<&str>,
    cursor: Option<&str>,
    page_size: Option<u32>,
) -> Result<SectionWindow, WindowError> {
    let requested = page_size.unwrap_or(DEFAULT_PROFILE_PAGE_SIZE);
    let page_size = requested.clamp(1, CEILING_PROFILE_PAGE_SIZE) as usize;

    let (section, offset) = match (section, cursor) {
        (None, None) => (None, 0),
        (Some(s), None) => (Some(validated_section(s)?), 0),
        (section, Some(c)) => {
            let (cursor_section, offset) = parse_cursor(c)?;
            if let Some(s) = section {
                let s = validated_section(s)?;
                if s != cursor_section {
                    return Err(WindowError::MalformedCursor(c.to_string()));
                }
            }
            (Some(cursor_section), offset)
        }
    };

    Ok(SectionWindow {
        section,
        offset,
        page_size,
    })
}

fn validated_section(s: &str) -> Result<String, WindowError> {
    if SECTIONS.contains(&s) {
        Ok(s.to_string())
    } else {
        Err(WindowError::UnknownSection(s.to_string()))
    }
}

fn parse_cursor(cursor: &str) -> Result<(String, usize), WindowError> {
    let (section, offset) = cursor
        .split_once('@')
        .ok_or_else(|| WindowError::MalformedCursor(cursor.to_string()))?;
    let section =
        validated_section(section).map_err(|_| WindowError::MalformedCursor(cursor.to_string()))?;
    let offset: usize = offset
        .parse()
        .map_err(|_| WindowError::MalformedCursor(cursor.to_string()))?;
    Ok((section, offset))
}

/// One window onto one section, with the total taken from the **unwindowed** input.
///
/// This is the only place in the workbench that turns a complete vector into a `Page`. An offset
/// past the end yields an empty window with a `None` cursor and the true total — a client that
/// over-pages sees "nothing more", never an error and never a wrapped-around first page.
pub fn section_page<T, V>(
    name: &str,
    rows: &[T],
    window: &SectionWindow,
    map: impl Fn(&T) -> V,
) -> Page<V> {
    let total = rows.len();
    let offset = if window.section.as_deref() == Some(name) {
        window.offset.min(total)
    } else {
        0
    };
    let end = (offset + window.page_size).min(total);
    let mapped: Vec<V> = rows[offset..end].iter().map(map).collect();
    let next_cursor = if end < total {
        Some(format!("{name}@{end}"))
    } else {
        None
    };
    Page {
        rows: mapped,
        next_cursor,
        total: total as u64,
    }
}

/// An `EntityRef` for `id`, resolved through the boot-time label index (NAV-06's own name source).
///
/// The fallback names the entity by its id rather than inventing a display name: a pivot whose
/// label is the id is legible and correct, whereas a prettified id is a name the graph never
/// stated. `kind` falls back to `"Entity"`, the `RecordKind` variant every graph node id is
/// addressable as.
pub fn entity_ref_for(label_index: &LabelIndex, id: &str) -> EntityRef {
    entity_ref_for_kind(label_index, id, RecordKind::Entity)
}

/// [`entity_ref_for`], but naming the class this id is known to belong to when the label index
/// cannot answer.
///
/// The index is built from the replayed hot graph, so it normally holds the node and supplies both
/// name and kind. When it does not — a node the profile read path reached from `graph_events` that
/// the snapshot has not got — the bare `entity_ref_for` fallback would label a `Location` node
/// `"Entity"`. The caller knows better, because it knows which section it is mapping, so it says.
///
/// `RecordKind::name` and `node_kind_name` agree on `"Entity"`, `"Contract"` and `"Location"` by
/// construction (D-06), so the fallback kind and the indexed kind are the same string for every
/// class this is called with — the fallback degrades the *name*, never the class.
fn entity_ref_for_kind(label_index: &LabelIndex, id: &str, kind: RecordKind) -> EntityRef {
    label_index
        .index_of(id)
        .and_then(|idx| label_index.entity_ref_at(idx))
        .unwrap_or_else(|| EntityRef {
            id: id.to_string(),
            name: id.to_string(),
            kind: kind.name().to_string(),
        })
}

fn status_view(status: &EntityStatus) -> EntityStatusView {
    match status {
        EntityStatus::Active => EntityStatusView::Active,
        EntityStatus::Dissolved(on) => EntityStatusView::Dissolved {
            on: on.format("%Y-%m-%d").to_string(),
        },
        EntityStatus::InLiquidation(on) => EntityStatusView::InLiquidation {
            on: on.format("%Y-%m-%d").to_string(),
        },
    }
}

fn officer_row(r: &DirectorRow) -> OfficerRowView {
    OfficerRowView {
        name: r.name.clone(),
        role: r.role.clone(),
        appointed: r.appointed.clone(),
        resigned: r.resigned.clone(),
        // No id on `DirectorRow` — see this module's "Pivots" note.
        pivot: None,
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn address_row(label_index: &LabelIndex, r: &AddressRow) -> AddressRowView {
    AddressRowView {
        postcode: r.postcode.clone(),
        address_line_1: r.address_line_1.clone(),
        locality: r.locality.clone(),
        // `AddressRow::id` is the `LocatedAt` destination — a real `Location` node id, carried
        // from the graph rather than derived from the postcode. Always `Some`: the read path
        // cannot build an address row without it.
        pivot: Some(entity_ref_for_kind(
            label_index,
            &r.id,
            RecordKind::Location,
        )),
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn contract_row(label_index: &LabelIndex, r: &ContractRow) -> ContractRowView {
    ContractRowView {
        name: r.name.clone(),
        value_gbp: r.value_gbp,
        status: r.status.clone(),
        published: r.published.clone(),
        ward_code: r.ward_code.clone(),
        tender_ref: r.tender_ref.clone(),
        // `ContractRow::application_ref` is deliberately NOT carried: 45-01 shaped
        // `ContractRowView` without it, and since the D-03 split it is `None` on every row
        // assembled from the store. It survives in core as the mis-routing detector 45-03's test
        // asserts against.
        //
        // `ContractRow::id` IS carried, as the pivot: it is the `AwardedTo` destination id.
        pivot: Some(entity_ref_for_kind(
            label_index,
            &r.id,
            RecordKind::Contract,
        )),
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn planning_row(
    label_index: &LabelIndex,
    r: &PlanningApplicationRow,
) -> PlanningApplicationRowView {
    PlanningApplicationRowView {
        name: r.name.clone(),
        status: r.status.clone(),
        published: r.published.clone(),
        ward_code: r.ward_code.clone(),
        application_ref: r.application_ref.clone(),
        // `RecordKind::Contract`, not a planning-specific kind: D-03 splits the section, not the
        // node. `planning:…` and `CF:…` are both `NodeWeight::Contract`, and the id prefix beside
        // the kind is what distinguishes them.
        pivot: Some(entity_ref_for_kind(
            label_index,
            &r.id,
            RecordKind::Contract,
        )),
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn property_row(r: &PropertyRow) -> PropertyRowView {
    PropertyRowView {
        label: r.label.clone(),
        pivot: None,
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn linked_record_row(r: &LinkedRecordRow) -> LinkedRecordRowView {
    LinkedRecordRowView {
        label: r.label.clone(),
        pivot: None,
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn beneficial_owner_row(r: &BeneficialOwnerRow) -> BeneficialOwnerRowView {
    BeneficialOwnerRowView {
        name: r.name.clone(),
        natures_of_control: r.natures_of_control.clone(),
        pivot: None,
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn disqualification_row(r: &DisqualificationRow) -> DisqualificationRowView {
    DisqualificationRowView {
        officer_name: r.officer_name.clone(),
        case_number: r.case_number.clone(),
        disqualified_from: r.disqualified_from.clone(),
        disqualified_until: r.disqualified_until.clone(),
        pivot: None,
        provenance: r.provenance.clone(),
        temporal: r.temporal.clone(),
    }
}

fn osint_link_row(label_index: &LabelIndex, r: &OsintLink) -> OsintLinkRowView {
    OsintLinkRowView {
        target_node_id: r.target_node_id.clone(),
        source_system: r.source_system.clone(),
        confidence: r.confidence,
        resolution_type: r.resolution_type.clone(),
        // A real id, so a real pivot. `OsintLink::status` is NOT carried: 45-01 put the tier on
        // the row's provenance so there is one place to read it from.
        pivot: Some(entity_ref_for(label_index, &r.target_node_id)),
        provenance: r.provenance.clone(),
    }
}

fn related_row(label_index: &LabelIndex, r: &RelatedEntity) -> RelatedEntityRowView {
    // `GB-COH:{number}` is the id `queries.rs` itself builds when matching a `CompanyNumber` ref
    // against a profile, so this is a reconstruction of a known id rather than an invented one. A
    // related entity with no CH number (a director slug) has no id on the row and no pivot.
    let pivot = r
        .ch_number
        .as_ref()
        .map(|n| entity_ref_for(label_index, &format!("GB-COH:{n}")));
    RelatedEntityRowView {
        name: r.name.clone(),
        ch_number: r.ch_number.clone(),
        status: status_view(&r.status),
        shared_connection: SharedConnectionView {
            kind: r.shared_connection.kind.clone(),
            value: r.shared_connection.value.clone(),
        },
        pivot,
        provenance: r.provenance.clone(),
    }
}

fn lpa_row(r: &LpaPresence) -> LpaPresenceView {
    LpaPresenceView {
        lpa_code: r.lpa_code.clone(),
        lpa_name: r.lpa_name.clone(),
        contract_count: r.contract_count as u64,
        planning_application_count: r.planning_application_count as u64,
    }
}

fn alert_row(r: &AlertRecord) -> AlertRowView {
    AlertRowView {
        id: r.id.clone(),
        motif_type: r.motif_type.clone(),
        score: r.score,
        detected_at: r.detected_at.clone(),
        ward_code: r.ward_code.clone(),
        withdrawn_at: r.withdrawn_at.clone(),
        withdrawn_reason: r.withdrawn_reason.clone(),
        provenance: r.provenance.clone(),
    }
}

fn osint_alert_row(r: &OsintAlertRow) -> OsintAlertRowView {
    OsintAlertRowView {
        motif_type: r.motif_type.clone(),
        severity: r.severity.clone(),
        provenance: r.provenance.clone(),
    }
}

fn sanctions_view(s: &SanctionsStatus) -> SanctionsStatusView {
    SanctionsStatusView {
        on_list: s.on_list,
        match_name: s.match_name.clone(),
        match_score: s.match_score,
        dataset: s.dataset.clone(),
        provenance: s.provenance.clone(),
    }
}

fn outcome_view(outcome: FindingsOutcome) -> FindingsOutcomeView {
    match outcome {
        FindingsOutcome::Named => FindingsOutcomeView::Named,
        FindingsOutcome::NotNamed => FindingsOutcomeView::NotNamed,
        FindingsOutcome::NotSearched => FindingsOutcomeView::NotSearched,
    }
}

/// A probe row's finding, re-joined to the `Finding` it was built from.
///
/// `FindingRef` carries the verdict's **discriminant string** (45-05's deliberate choice —
/// "the discriminant, not the `Verdict` itself, which carries a `Coverage` this row has no use
/// for"), while `FindingRowView.verdict` is a full [`VerdictView`] (45-01's deliberate choice —
/// "reusing this crate's existing mirror so a finding on the profile and the same finding on the
/// Analysis page cannot render as different verdicts"). Both are right and they do not meet, so
/// the handler re-joins the row to the finding slice it was built from, in the same request.
///
/// A miss is structurally impossible — `findings_naming_entity` built the row *from* this slice —
/// so it is reported as an error rather than papered over with a plausible-looking verdict.
fn finding_row(
    entries: &[ReportEntry],
    probe: &str,
    row: &FindingRef,
) -> Result<FindingRowView, String> {
    let entry = entries
        .iter()
        .find(|e| e.finding.probe == probe && e.finding.summary == row.summary)
        .ok_or_else(|| {
            format!(
                "findings row {probe:?}/{:?} could not be re-joined to the finding it was built \
                 from — the profile would have had to invent a verdict, which it will not do",
                row.summary
            )
        })?;
    Ok(FindingRowView {
        probe: probe.to_string(),
        summary: row.summary.clone(),
        verdict: crate::verdict_conversion::to_verdict_view(
            &entry.finding.verdict,
            &entry.population_source,
        ),
        matched_via: row.named_by.name().to_string(),
        provenance: row.provenance.clone(),
    })
}

/// Build the "Findings & alerts" section (PIV-02, D-05).
///
/// The three pages come from three places on purpose:
///
/// * `findings` — the `Probe`-origin rows of the reverse index (45-05).
/// * `alerts` — `EntityProfile::alerts`, which carries the id, detection time, ward and
///   withdrawal fields a `FindingRef` does not. `assemble_entity_profile` builds it with the same
///   scan and the same withdrawn-alert filter as `alerts_naming_entity`, so this is the same
///   population the index's `MotifAlert` rows came from, not a second one.
/// * `osint_alerts` — `EntityProfile::osint_alerts`.
///
/// `outcome` is carried from the index verbatim, never recomputed. Note that it speaks for the
/// whole index: an entity named only by a motif alert is `Named` while the findings page is empty,
/// which is correct — the alerts page beside it is what named it.
fn findings_section(
    profile: &EntityProfile,
    index: &FindingsByEntity,
    entries: &[ReportEntry],
    window: &SectionWindow,
) -> Result<FindingsSectionView, String> {
    let probe_rows: Vec<&FindingRef> = index
        .rows
        .iter()
        .filter(|r| matches!(r.origin, FindingOrigin::Probe { .. }))
        .collect();

    let page = section_page("findings", &probe_rows, window, |r| *r);
    let mut rows = Vec::with_capacity(page.rows.len());
    for row in &page.rows {
        let FindingOrigin::Probe { name, .. } = &row.origin else {
            unreachable!("filtered to Probe origins above");
        };
        rows.push(finding_row(entries, name, row)?);
    }

    Ok(FindingsSectionView {
        findings: Page {
            rows,
            next_cursor: page.next_cursor,
            total: page.total,
        },
        alerts: section_page("alerts", &profile.alerts, window, alert_row),
        osint_alerts: section_page(
            "osint_alerts",
            &profile.osint_alerts,
            window,
            osint_alert_row,
        ),
        outcome: outcome_view(index.outcome),
        probes_total: index.coverage.probes_total() as u32,
        probes_unable_to_name_an_entity: index.coverage.probes_not_searchable() as u32,
    })
}

/// The whole projection. `index`/`findings` come from the caller because nothing persists a
/// `Finding` — see `entity::profile_handler`.
pub fn to_dto(
    profile: &EntityProfile,
    label_index: &LabelIndex,
    index: &FindingsByEntity,
    entries: &[ReportEntry],
    window: &SectionWindow,
) -> Result<EntityProfileDto, String> {
    let subject = entity_ref_for(label_index, &profile.id);

    // One provenance for all three header claims, because there is one event — `identity_provenance`'s
    // own doc comment on `EntityProfile` says so, and giving each identifier its own copy would let
    // the header answer one question three ways.
    let mut identifiers = Vec::new();
    if let Some(ch) = &profile.ch_number {
        identifiers.push(IdentifierFactView {
            label: "Companies House number".to_string(),
            value: ch.clone(),
            provenance: profile.identity_provenance.clone(),
        });
    }
    if let Some(inc) = &profile.incorporation_date {
        identifiers.push(IdentifierFactView {
            label: "Incorporated".to_string(),
            value: inc.format("%Y-%m-%d").to_string(),
            provenance: profile.identity_provenance.clone(),
        });
    }

    Ok(EntityProfileDto {
        id: profile.id.clone(),
        name: profile.name.clone(),
        kind: subject.kind,
        status: StatusFactView {
            value: status_view(&profile.status),
            provenance: profile.identity_provenance.clone(),
        },
        identifiers,
        officers: section_page("officers", &profile.directors, window, officer_row),
        addresses: section_page("addresses", &profile.addresses, window, |r| {
            address_row(label_index, r)
        }),
        contracts: section_page("contracts", &profile.contracts, window, |r| {
            contract_row(label_index, r)
        }),
        planning_applications: section_page(
            "planning_applications",
            &profile.planning_applications,
            window,
            |r| planning_row(label_index, r),
        ),
        property: section_page("property", &profile.owns_property, window, property_row),
        osint_links: section_page("osint_links", &profile.osint_links, window, |r| {
            osint_link_row(label_index, r)
        }),
        beneficial_owners: section_page(
            "beneficial_owners",
            &profile.beneficial_owners,
            window,
            beneficial_owner_row,
        ),
        disqualifications: section_page(
            "disqualifications",
            &profile.disqualifications,
            window,
            disqualification_row,
        ),
        trustee_of: section_page("trustee_of", &profile.trustee_of, window, linked_record_row),
        donates_to: section_page("donates_to", &profile.donates_to, window, linked_record_row),
        offshore_links: section_page(
            "offshore_links",
            &profile.offshore_links,
            window,
            linked_record_row,
        ),
        sanctions: profile.sanctions_status.as_ref().map(sanctions_view),
        related: section_page("related", &profile.related_entities, window, |r| {
            related_row(label_index, r)
        }),
        lpa_presence: profile.lpa_presence.iter().map(lpa_row).collect(),
        findings: findings_section(profile, index, entries, window)?,
        as_at: profile.as_at.map(|d| d.format("%Y-%m-%d").to_string()),
        // `EntityProfile::sub_candidate_links` is not mapped and must never be: ENR-11/D-13, and
        // `EntityProfileDto` has no field to put it in (45-01 left it out of the vocabulary).
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The two `EntityRef::kind` vocabularies agree wherever they overlap.**
    ///
    /// `EntityRef::kind` is filled by two producers: `RecordKind::name` (probe evidence refs, and
    /// this module's pivot fallbacks) and `graph::arrow_export::node_kind_name` (the label index,
    /// the entity search, the profile header, and this module's pivots whenever the index
    /// resolves). D-06 chose to extend `RecordKind` rather than mirror it, and the whole value of
    /// that choice is that one node class has ONE spelling on this field no matter which producer
    /// filled it. Nothing in the type system enforces it, so this does.
    ///
    /// The overlap is exactly two classes. `CompanyNumber`/`IdentityLink` name things that are not
    /// graph nodes and have no `NodeWeight`; `Officer`, `Ward`, `Psc`, `Charity`, `Property`,
    /// `OffshoreEntity` and `Disqualification` are node classes no `RecordKind` names. Those are
    /// asserted as absent from the other vocabulary rather than left unmentioned, so this test
    /// notices a variant being added on either side.
    ///
    /// PROVE THIS CAN FAIL: spell the schema variant `RecordKind::Address` (returning `"Address"`
    /// from `RecordKind::name`) — the natural reading of PIV-03's wording — and the `Location`
    /// assertion fails with `"Address" != "Location"`. That mutation was run.
    #[test]
    fn record_kind_and_node_kind_spell_the_two_classes_they_share_identically() {
        use crate::graph::arrow_export::node_kind_name;
        use urban_integrity_schema::types::node::NodeWeight;

        // Built through the same tagged-JSON form `graph_events` stores, so the two nodes are the
        // ones the read path would actually hand this module, not hand-picked struct literals.
        let contract_node: NodeWeight =
            serde_json::from_str(r#"{"type":"Contract","name":"X9999 CF Award"}"#)
                .expect("Contract node weight");
        let location_node: NodeWeight =
            serde_json::from_str(r#"{"type":"Location","name":"1 X9999 Street"}"#)
                .expect("Location node weight");

        assert_eq!(
            RecordKind::Contract.name(),
            node_kind_name(&contract_node),
            "a contract node must be one word on the wire, not `Contract` from one producer and              something else from the other"
        );
        assert_eq!(
            RecordKind::Location.name(),
            node_kind_name(&location_node),
            "the address node class is `Location` in the graph; `Address` is the name of the              profile SECTION and must not become a second spelling of this class"
        );
        assert_eq!(RecordKind::Entity.name(), "Entity");

        // Non-vacuity: the comparison above would also pass if `node_kind_name` returned the empty
        // string for everything.
        assert!(!node_kind_name(&contract_node).is_empty());
        assert!(!node_kind_name(&location_node).is_empty());

        // The non-overlap, stated so a new variant on either side is noticed rather than assumed.
        for probe_only in [RecordKind::CompanyNumber, RecordKind::IdentityLink] {
            assert!(
                ![
                    node_kind_name(&contract_node),
                    node_kind_name(&location_node),
                    "Entity",
                    "Officer",
                    "Ward",
                    "PoliticalActor",
                    "Psc",
                    "Charity",
                    "Property",
                    "OffshoreEntity",
                    "Disqualification",
                ]
                .contains(&probe_only.name()),
                "{} names something that is not a graph node and must not collide with a node                  class name",
                probe_only.name()
            );
        }
    }

    /// A window taken from a section shorter than the page reports the whole section and no
    /// cursor: `total` is the section, not the window.
    #[test]
    fn a_short_section_is_one_page_with_no_cursor() {
        let rows = vec![1u32, 2, 3];
        let page = section_page("contracts", &rows, &SectionWindow::default(), |n| *n);
        assert_eq!(page.rows, vec![1, 2, 3]);
        assert_eq!(page.total, 3);
        assert_eq!(page.next_cursor, None);
    }

    /// The total is the unwindowed length, and the cursor names the next offset in the section it
    /// belongs to.
    ///
    /// PROVE THIS CAN FAIL: computing `total` from `mapped.len()` instead of `rows.len()` in
    /// [`section_page`] makes the `total` assertion fail with `2`. That mutation was run.
    #[test]
    fn a_windowed_section_reports_the_whole_total_and_a_next_cursor() {
        let rows: Vec<u32> = (0..5).collect();
        let window = SectionWindow {
            section: None,
            offset: 0,
            page_size: 2,
        };
        let page = section_page("contracts", &rows, &window, |n| *n);
        assert_eq!(page.rows, vec![0, 1]);
        assert_eq!(page.total, 5);
        assert_eq!(page.next_cursor.as_deref(), Some("contracts@2"));
    }

    /// A cursor moves only the section it names. Every other section stays at its first window —
    /// which is what lets one response carry a coherent whole profile.
    #[test]
    fn a_cursor_moves_only_the_section_it_names() {
        let rows: Vec<u32> = (0..5).collect();
        let window = window_from_request(None, Some("contracts@2"), Some(2)).expect("valid window");
        let moved = section_page("contracts", &rows, &window, |n| *n);
        let untouched = section_page("officers", &rows, &window, |n| *n);
        assert_eq!(moved.rows, vec![2, 3]);
        assert_eq!(untouched.rows, vec![0, 1]);
    }

    /// Paging past the end is an empty last window, not an error and not a wrapped first page.
    #[test]
    fn an_offset_past_the_end_is_an_empty_last_page() {
        let rows: Vec<u32> = (0..3).collect();
        let window = window_from_request(None, Some("contracts@99"), None).expect("valid window");
        let page = section_page("contracts", &rows, &window, |n| *n);
        assert!(page.rows.is_empty());
        assert_eq!(page.total, 3);
        assert_eq!(page.next_cursor, None);
    }

    /// The page size is the server's, not the browser's.
    #[test]
    fn page_size_is_clamped_to_the_shared_ceiling() {
        let window = window_from_request(None, None, Some(10_000)).expect("valid window");
        assert_eq!(window.page_size, CEILING_PROFILE_PAGE_SIZE as usize);
        let zero = window_from_request(None, None, Some(0)).expect("valid window");
        assert_eq!(zero.page_size, 1, "a zero-row page never terminates");
    }

    /// An unknown section and a malformed cursor are both rejected, and a cursor for one section
    /// passed alongside a different `section` parameter is rejected too.
    #[test]
    fn a_cursor_for_the_wrong_section_is_rejected_not_applied() {
        assert!(matches!(
            window_from_request(Some("nonexistent"), None, None),
            Err(WindowError::UnknownSection(_))
        ));
        assert!(matches!(
            window_from_request(None, Some("contracts"), None),
            Err(WindowError::MalformedCursor(_))
        ));
        assert!(matches!(
            window_from_request(Some("officers"), Some("contracts@2"), None),
            Err(WindowError::MalformedCursor(_))
        ));
    }
}
