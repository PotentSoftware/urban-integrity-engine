//! The profile's sections: their fixed order, their labels, their totals and the index chip row
//! (the phase 45 UI spec "Entity profile page", items 3 and 4).
//!
//! # Sixteen sections, not eight
//!
//! The spec fixes an eight-row order — identifiers, officers, addresses, contracts, planning
//! applications, property, OSINT links, findings — and PIV-02 names the same eight. But
//! [`EntityProfileDto`] carries **sixteen** populated-capable sections, and 45-04 measured two of
//! the missing ones as live on this corpus (related entities: 41 rows across nine subjects;
//! trustee-of and donates-to: one each). A section that exists on the wire and nowhere on screen is
//! silent data loss — this project's own `projector.rs` lost 30,836 rows of
//! `application_received_date` exactly that way, with no error anywhere.
//!
//! So the eight keep their specified order and the remaining eight are rendered **between OSINT
//! links and findings**, not appended after it. Findings stays last because the spec's reason for
//! putting it there is not its index but its subject matter: it is *about* the sections above it.
//!
//! The spec's eighth section, "Findings & alerts", is rendered as **three** adjacent sections —
//! probe findings, motif alerts and OSINT alerts. They are three different populations from three
//! different producers (`probe`, `detector`, and the OSINT adapters), the read path pages them
//! separately, and only the first is subject to D-05's evidence-model empty state. Summing them
//! into one index count would state a number that is not a count of anything.
//!
//! # Where a glossary token lives in a section name
//!
//! Two section names carry one (`OSINT links`, `LPA presence`), and D-03/NAV-04 require a rendered
//! token to sit inside `<Abbr>`. [`section_label_parts`] therefore returns the name in three pieces
//! — prefix, optional token, suffix — so the header and the index chip render
//! `{prefix}<Abbr/>{suffix}` while [`section_noun`] reassembles the plain string that empty-state
//! sentences interpolate. The token itself is [`crate::tool_rail::OSINT_ABBR`], imported rather
//! than retyped, so the canvas legend and this page cannot come to spell it differently.

use leptos::prelude::*;
use urban_integrity_workbench_dto::entity_profile::{FindingsOutcomeView, LinkedRecordRowView};
use urban_integrity_workbench_dto::{
    AsAtStatus, EffectiveRange, EntityProfileDto, EntityRef, Page, Provenance, Temporal,
    VerdictView,
};

use crate::abbr::Abbr;
use crate::disambiguation::status_label;
use crate::profile::pivot::pivot_href;
use crate::profile::provenance_chip::{
    section_provenance, shows_unattributed_chips, ProvenanceChip, SectionProvenanceMarker,
};
use crate::profile::{Density, COMPACT_ROWS_PER_SECTION};
use crate::tool_rail::OSINT_ABBR;
use crate::verdict_badge::VerdictBadge;

/// The glossary token for a Local Planning Authority, in the `*_ABBR` form
/// `crate::help::CLI_ABBR` established.
pub const LPA_ABBR: &str = "LPA";

/// One section of the profile, in render order.
///
/// [`SECTION_ORDER`] is the render order, and it is a hand-written list rather than a derivation
/// from this enum's discriminants — so a variant added here without a place there would silently
/// never render. `every_section_is_ordered_exactly_once` is what makes that a test failure instead,
/// by asserting the count as a number as well as by presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionId {
    Identifiers,
    Officers,
    Addresses,
    Contracts,
    PlanningApplications,
    Property,
    OsintLinks,
    BeneficialOwners,
    Disqualifications,
    Related,
    TrusteeOf,
    DonatesTo,
    OffshoreLinks,
    Sanctions,
    LpaPresence,
    Findings,
    Alerts,
    OsintAlerts,
}

/// Every section, in the order they render. See [`SectionId`].
pub const SECTION_ORDER: &[SectionId] = &[
    SectionId::Identifiers,
    SectionId::Officers,
    SectionId::Addresses,
    SectionId::Contracts,
    SectionId::PlanningApplications,
    SectionId::Property,
    SectionId::OsintLinks,
    SectionId::BeneficialOwners,
    SectionId::Disqualifications,
    SectionId::Related,
    SectionId::TrusteeOf,
    SectionId::DonatesTo,
    SectionId::OffshoreLinks,
    SectionId::Sanctions,
    SectionId::LpaPresence,
    SectionId::Findings,
    SectionId::Alerts,
    SectionId::OsintAlerts,
];

/// A section name split for rendering: `(prefix, glossary token, suffix)`. See the module doc.
pub fn section_label_parts(
    section: SectionId,
) -> (&'static str, Option<&'static str>, &'static str) {
    match section {
        SectionId::Identifiers => ("Identifiers & status", None, ""),
        SectionId::Officers => ("Officers", None, ""),
        SectionId::Addresses => ("Addresses", None, ""),
        SectionId::Contracts => ("Contracts", None, ""),
        SectionId::PlanningApplications => ("Planning applications", None, ""),
        SectionId::Property => ("Property", None, ""),
        SectionId::OsintLinks => ("", Some(OSINT_ABBR), " links"),
        SectionId::BeneficialOwners => ("Beneficial owners", None, ""),
        SectionId::Disqualifications => ("Disqualifications", None, ""),
        SectionId::Related => ("Related entities", None, ""),
        SectionId::TrusteeOf => ("Trustee of", None, ""),
        SectionId::DonatesTo => ("Donates to", None, ""),
        SectionId::OffshoreLinks => ("Offshore links", None, ""),
        SectionId::Sanctions => ("Sanctions", None, ""),
        SectionId::LpaPresence => ("", Some(LPA_ABBR), " presence"),
        SectionId::Findings => ("Probe findings", None, ""),
        SectionId::Alerts => ("Motif alerts", None, ""),
        SectionId::OsintAlerts => ("", Some(OSINT_ABBR), " alerts"),
    }
}

/// The section's name as one string — the section header's accessible text and the noun every
/// empty-state sentence interpolates. Assembled from [`section_label_parts`] so the two can never
/// disagree.
pub fn section_noun(section: SectionId) -> String {
    let (prefix, token, suffix) = section_label_parts(section);
    format!("{prefix}{}{suffix}", token.unwrap_or_default())
}

/// The name this section is paged under — the `section=` parameter of a **Show more** press, and
/// one of `crates/workbench`'s `profile_map::SECTIONS`. `None` for the three sections that are not
/// a `Page` at all: identifiers, sanctions and LPA presence are whole-of-section values the read
/// path returns unwindowed.
pub fn section_cursor_name(section: SectionId) -> Option<&'static str> {
    Some(match section {
        SectionId::Identifiers | SectionId::Sanctions | SectionId::LpaPresence => return None,
        SectionId::Officers => "officers",
        SectionId::Addresses => "addresses",
        SectionId::Contracts => "contracts",
        SectionId::PlanningApplications => "planning_applications",
        SectionId::Property => "property",
        SectionId::OsintLinks => "osint_links",
        SectionId::BeneficialOwners => "beneficial_owners",
        SectionId::Disqualifications => "disqualifications",
        SectionId::Related => "related",
        SectionId::TrusteeOf => "trustee_of",
        SectionId::DonatesTo => "donates_to",
        SectionId::OffshoreLinks => "offshore_links",
        SectionId::Findings => "findings",
        SectionId::Alerts => "alerts",
        SectionId::OsintAlerts => "osint_alerts",
    })
}

/// How many rows the section has **in full** — the `Page::total` the read path matched, not the
/// number in this window. The section header carries it so an analyst can see what was cut.
pub fn section_total(profile: &EntityProfileDto, section: SectionId) -> u64 {
    match section {
        // +1 for the status row, which `section_rows` puts at the head of this section. Without
        // it the caption reads "Showing 1 of 0." on an entity whose only identifier fact is its
        // status — a window larger than the thing it is a window onto.
        SectionId::Identifiers => profile.identifiers.len() as u64 + 1,
        SectionId::Officers => profile.officers.total,
        SectionId::Addresses => profile.addresses.total,
        SectionId::Contracts => profile.contracts.total,
        SectionId::PlanningApplications => profile.planning_applications.total,
        SectionId::Property => profile.property.total,
        SectionId::OsintLinks => profile.osint_links.total,
        SectionId::BeneficialOwners => profile.beneficial_owners.total,
        SectionId::Disqualifications => profile.disqualifications.total,
        SectionId::Related => profile.related.total,
        SectionId::TrusteeOf => profile.trustee_of.total,
        SectionId::DonatesTo => profile.donates_to.total,
        SectionId::OffshoreLinks => profile.offshore_links.total,
        SectionId::Sanctions => u64::from(profile.sanctions.is_some()),
        SectionId::LpaPresence => profile.lpa_presence.len() as u64,
        SectionId::Findings => profile.findings.findings.total,
        SectionId::Alerts => profile.findings.alerts.total,
        SectionId::OsintAlerts => profile.findings.osint_alerts.total,
    }
}

/// The section index's entries: every section with its total, in render order.
///
/// **Every section, including the empty ones.** The phase 45 UI spec's reason for the index is "so an
/// analyst can see what this entity *has* before scrolling", and a zero is part of that answer —
/// an index that listed only the populated sections would make an empty Contracts section
/// indistinguishable from a Contracts section that was never rendered.
pub fn section_index_entries(profile: &EntityProfileDto) -> Vec<(SectionId, u64)> {
    SECTION_ORDER
        .iter()
        .map(|section| (*section, section_total(profile, *section)))
        .collect()
}

/// The fragment id a section anchors at, so the index chip can jump to it.
pub fn section_anchor(section: SectionId) -> String {
    section_noun(section)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A section name with its glossary token inside `<Abbr>` (D-03/NAV-04). See the module doc for
/// why the name arrives in three pieces.
#[component]
#[allow(non_snake_case)]
pub fn SectionLabel(section: SectionId) -> impl IntoView {
    let (prefix, token, suffix) = section_label_parts(section);
    view! {
        <>
            {prefix}
            {token.map(|t| view! { <Abbr token=t.to_string() /> })}
            {suffix}
        </>
    }
}

/// The section index (the phase 45 UI spec item 3), as a horizontal chip row **at every width**.
///
/// The spec's sticky 240px rail at >=1280px is deliberately not built — see
/// `crate::profile`'s module doc for the reasoning and for the phase 45 UI spec Open Question 1's answer.
#[component]
#[allow(non_snake_case)]
pub fn SectionIndex(profile: EntityProfileDto) -> impl IntoView {
    let entries = section_index_entries(&profile);
    view! {
        <nav
            aria-label="Profile sections"
            style="display: flex; flex-wrap: wrap; gap: var(--space-sm);"
        >
            {entries.into_iter().map(|(section, total)| {
                let href = format!("#{}", section_anchor(section));
                view! {
                    <a
                        class="text-label"
                        href=href
                        style="display: inline-flex; align-items: center; gap: var(--space-xs); min-height: 44px; padding: 0 var(--space-sm); border: 1px solid var(--border); border-radius: 6px; color: var(--muted-foreground); text-decoration: none;"
                    >
                        <SectionLabel section=section />
                        <span class="font-mono">{total.to_string()}</span>
                    </a>
                }
            }).collect_view()}
        </nav>
    }
}

// ── Rows ────────────────────────────────────────────────────────────────────────────────────────
//
// One row model and one table renderer for all eighteen sections. The alternative — a bespoke
// component per section — is eighteen places for the pivot policy, the paging caption, the as-at
// marking and the provenance column to drift apart, and 45-13 mounts every one of them again at a
// second density. `section_columns`/`section_rows` are the only per-section code, and
// `every_row_has_one_cell_per_column` pins the pair together.

/// What a table cell shows when the record states no value.
///
/// An em dash, not "Unknown" and not a blank: blank reads as a rendering fault, and "Unknown" is a
/// claim about the world rather than about the record. The same distinction
/// `EffectiveRange::Unrecorded` makes one layer down.
pub const NOT_RECORDED: &str = "—";

/// The chip on a row the as-at filter has positively placed outside its date (PIV-04's worked
/// example: "a row whose tenure does not span the as-at date is visually distinguished from one
/// that does").
///
/// A text chip plus `--muted-foreground`, deliberately **not** the canvas's `0.12` opacity dim —
/// the only dimming this workbench had inherited, and one that would fail text contrast
/// (the phase 45 UI spec checker finding 4, which declared no mechanism).
pub const OUTSIDE_AS_AT: &str = "outside as at";

/// The chip on a row the as-at filter **could not test**: it records no usable endpoint, so it
/// comes back [`AsAtStatus::Undated`] — retained under the null-range rule and labelled, so the
/// filter is never read as having vouched for it (45-06).
///
/// **The common case, not an edge case.** Every `Directs` and every `LocatedAt` edge in this store
/// is undated, so whole sections arrive in this state; 45-06's hand-over says in as many words that
/// "a design that treats `Undated` as the rare case will be wrong on this corpus". A chip on every
/// row of a section is therefore the correct rendering of a filter that tested nothing there, and
/// the alternative — saying nothing — is the silent-population-change this requirement exists to
/// prevent.
///
/// Not muted, unlike [`OUTSIDE_AS_AT`]. Dimming says "this is outside the window you asked for";
/// an untested row may well have been in force, and dimming it would assert the opposite of what
/// the rule says.
///
/// The cell's own text is not a substitute for this chip. `range_text` prints `Undated` from the
/// *range* being `Unrecorded`, and a row can be `Undated` with a `Recorded` range whose endpoints
/// do not parse as dates (`as_at_status` in `crates/core`) — that row shows dates in the column and
/// was still never tested.
pub const NOT_TESTED: &str = "not tested";

/// A pounds-and-pence figure as a scannable `.font-mono` string: `£125,000`, or `£1,250.50` when
/// the record states pence. Never rounded to a friendlier number — a contract value is evidence.
pub fn format_money(value: f64) -> String {
    let negative = value < 0.0;
    let abs = value.abs();
    let whole = abs.trunc() as u64;
    let pence = ((abs - abs.trunc()) * 100.0).round() as u64;
    let mut digits: Vec<String> = Vec::new();
    let text = whole.to_string();
    let bytes = text.as_bytes();
    let mut i = bytes.len();
    while i > 3 {
        digits.push(text[i - 3..i].to_string());
        i -= 3;
    }
    digits.push(text[..i].to_string());
    digits.reverse();
    let grouped = digits.join(",");
    let sign = if negative { "-" } else { "" };
    if pence == 0 {
        format!("{sign}£{grouped}")
    } else {
        format!("{sign}£{grouped}.{pence:02}")
    }
}

/// A composite confidence in [0, 1] to two decimals. Two, not four: the score is a ranking aid, and
/// more digits than the resolution justifies read as more certainty than there is.
pub fn format_confidence(value: f64) -> String {
    format!("{value:.2}")
}

fn or_missing(value: Option<&String>) -> String {
    value.cloned().unwrap_or_else(|| NOT_RECORDED.to_string())
}

/// Joins the mono identifiers that belong under a row's primary text, dropping the absent ones.
/// An empty result is `None`, never a line of separators.
fn detail_line(parts: &[Option<String>]) -> Option<String> {
    let present: Vec<String> = parts.iter().flatten().cloned().collect();
    (!present.is_empty()).then(|| present.join(" · "))
}

/// One rendered row of any section.
///
/// `provenance` is `Option` for exactly one reason, and it is a reason the wire types state rather
/// than a convenience: [`LpaPresenceView`] deliberately carries none, because it is a count
/// spanning two datasets and naming one of them as *the* source would be the placeholder
/// attribution D-04 forbids. `None` therefore means "this row is not a single-source fact", which
/// the section marker says out loud — it never means "provenance was not wired up".
#[derive(Debug, Clone, PartialEq)]
pub struct RowView {
    /// The row's first column, and the pivot's label where there is one.
    pub primary: String,
    /// The row's own identifiers, rendered `.font-mono` beneath the primary text: a tender
    /// reference, a planning application reference, a ward code, a graph id, an alert withdrawal.
    pub detail: Option<String>,
    pub pivot: Option<EntityRef>,
    /// One value per column after the first. `every_row_has_one_cell_per_column` pins the length.
    pub cells: Vec<String>,
    pub provenance: Option<Provenance>,
    pub temporal: Option<Temporal>,
    /// The findings section's verdict, rendered through the existing `VerdictBadge` so a finding
    /// here and the same finding on the Analysis page cannot show different verdicts.
    pub verdict: Option<VerdictView>,
}

impl RowView {
    fn new(primary: impl Into<String>, cells: Vec<String>, provenance: Provenance) -> Self {
        Self {
            primary: primary.into(),
            detail: None,
            pivot: None,
            cells,
            provenance: Some(provenance),
            temporal: None,
            verdict: None,
        }
    }

    fn with_pivot(mut self, pivot: Option<EntityRef>) -> Self {
        self.pivot = pivot;
        self
    }

    fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    fn with_temporal(mut self, temporal: Temporal) -> Self {
        self.temporal = Some(temporal);
        self
    }
}

/// The section's column headers, the first of which labels the primary column.
pub fn section_columns(section: SectionId) -> &'static [&'static str] {
    match section {
        SectionId::Identifiers => &["Identifier", "Value"],
        SectionId::Officers => &["Officer", "Role"],
        SectionId::Addresses => &["Address", "Postcode"],
        SectionId::Contracts => &["Contract", "Value", "Status"],
        SectionId::PlanningApplications => &["Application", "Status"],
        SectionId::Property => &["Title"],
        SectionId::OsintLinks => &["Target", "Register", "Confidence"],
        SectionId::BeneficialOwners => &["Owner", "Nature of control"],
        SectionId::Disqualifications => &["Officer", "Case"],
        SectionId::Related => &["Entity", "Status", "Shared connection"],
        SectionId::TrusteeOf | SectionId::DonatesTo | SectionId::OffshoreLinks => &["Record"],
        SectionId::Sanctions => &["Check", "Result"],
        SectionId::LpaPresence => &["Authority", "Contracts", "Planning applications"],
        SectionId::Findings => &["Probe", "Finding", "Matched via"],
        SectionId::Alerts => &["Motif", "Score", "Detected"],
        SectionId::OsintAlerts => &["Motif", "Severity"],
    }
}

/// The temporal column's header.
///
/// **Not "In force".** 45-10 named it that; the column holds a *range the record states*, and
/// naming it after a force determination makes the header assert what the cells do not. Two ways
/// that reads wrong, and the second is the common case live:
///
/// * on an unfiltered profile nothing has been tested against anything — the wire says
///   [`AsAtStatus::NotFiltered`] rather than `InForce` precisely so the two are not confused
///   (45-06) — so a column headed "In force" claims a determination no filter made;
/// * an undated row's cell reads `Undated`, and "In force: Undated" is a contradiction of the one
///   rule this screen exists to state. 45-06 measured entire sections as undated on this corpus
///   (every `Directs` and every `LocatedAt` edge), so that contradiction would be the *majority*
///   rendering, not an edge case.
///
/// The phase 45 UI spec never names this header; its own words for what the column holds are "its
/// `effective_from`/`effective_to` range".
pub const RANGE_COLUMN: &str = "Date range";

/// Whether this section's rows carry an [`EffectiveRange`], and therefore a [`RANGE_COLUMN`].
///
/// Read off the DTO's own shape: the row types carrying `temporal` are exactly the ones whose facts
/// come from a relation with `meta.effective_from`/`meta.effective_to`. A section that has one
/// renders the range **whether or not an as-at filter is set** — the phase 45 UI spec's "the unfiltered
/// profile must not be the less informative screen".
pub fn section_has_temporal(section: SectionId) -> bool {
    matches!(
        section,
        SectionId::Officers
            | SectionId::Addresses
            | SectionId::Contracts
            | SectionId::PlanningApplications
            | SectionId::Property
            | SectionId::BeneficialOwners
            | SectionId::Disqualifications
            | SectionId::TrusteeOf
            | SectionId::DonatesTo
            | SectionId::OffshoreLinks
    )
}

/// The range a row states, as one cell. Never "present" or "ongoing" for an open end: this store
/// cannot distinguish "still in force" from "no end recorded" — `effective_to` is null on all
/// 511,754 edge events — so the cell says what the record says and no more.
pub fn range_text(temporal: &Temporal) -> String {
    match &temporal.range {
        EffectiveRange::Unrecorded { .. } => "Undated".to_string(),
        EffectiveRange::Recorded { from, to, .. } => {
            let from = from.as_deref().map(iso_date).unwrap_or(NOT_RECORDED);
            match to.as_deref().map(iso_date) {
                Some(to) => format!("{from} → {to}"),
                None => format!("{from} →"),
            }
        }
    }
}

/// The date half of an ISO-8601 value. The read path hands back both `"2024-06-11"` and
/// `"2022-07-06T00:00:00Z"` depending on which field answered, and neither a table column nor a
/// 20px provenance chip is the place for a midnight that means "no time was recorded".
///
/// Shared with [`crate::profile::provenance_chip`] so the two date renderings on this screen cannot
/// come apart.
pub fn iso_date(value: &str) -> &str {
    value.split('T').next().unwrap_or(value)
}

/// Every row of one section, in the read path's order.
pub fn section_rows(profile: &EntityProfileDto, section: SectionId) -> Vec<RowView> {
    match section {
        // The status claim leads the section it belongs to: "Identifiers & status" is one section
        // in the spec, and status is a fact with a source like any other (45-01 made
        // `StatusFactView` carry provenance for exactly this reason).
        SectionId::Identifiers => std::iter::once(RowView::new(
            "Status",
            vec![status_label(&profile.status.value)],
            profile.status.provenance.clone(),
        ))
        .chain(profile.identifiers.iter().map(|row| {
            RowView::new(
                row.label.clone(),
                vec![row.value.clone()],
                row.provenance.clone(),
            )
        }))
        .collect(),
        SectionId::Officers => profile
            .officers
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.name.clone(),
                    vec![or_missing(row.role.as_ref())],
                    row.provenance.clone(),
                )
                .with_pivot(row.pivot.clone())
                .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::Addresses => profile
            .addresses
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.address_line_1
                        .clone()
                        .or_else(|| row.postcode.clone())
                        .unwrap_or_else(|| NOT_RECORDED.to_string()),
                    vec![or_missing(row.postcode.as_ref())],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(std::slice::from_ref(&row.locality)))
                .with_pivot(row.pivot.clone())
                .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::Contracts => profile
            .contracts
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.name.clone(),
                    vec![
                        row.value_gbp
                            .map(format_money)
                            .unwrap_or_else(|| NOT_RECORDED.to_string()),
                        or_missing(row.status.as_ref()),
                    ],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(&[
                    row.tender_ref.clone(),
                    row.ward_code.clone(),
                ]))
                .with_pivot(row.pivot.clone())
                .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::PlanningApplications => profile
            .planning_applications
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.name.clone(),
                    vec![or_missing(row.status.as_ref())],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(&[
                    row.application_ref.clone(),
                    row.ward_code.clone(),
                ]))
                .with_pivot(row.pivot.clone())
                .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::Property => profile
            .property
            .rows
            .iter()
            .map(|row| {
                RowView::new(row.label.clone(), Vec::new(), row.provenance.clone())
                    .with_pivot(row.pivot.clone())
                    .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::OsintLinks => profile
            .osint_links
            .rows
            .iter()
            .map(|row| {
                let label = row
                    .pivot
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| row.target_node_id.clone());
                RowView::new(
                    label,
                    vec![row.source_system.clone(), format_confidence(row.confidence)],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(&[
                    Some(row.target_node_id.clone()),
                    Some(row.resolution_type.clone()),
                ]))
                .with_pivot(row.pivot.clone())
            })
            .collect(),
        SectionId::BeneficialOwners => profile
            .beneficial_owners
            .rows
            .iter()
            .map(|row| {
                let control = if row.natures_of_control.is_empty() {
                    NOT_RECORDED.to_string()
                } else {
                    row.natures_of_control.join(", ")
                };
                RowView::new(row.name.clone(), vec![control], row.provenance.clone())
                    .with_pivot(row.pivot.clone())
                    .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::Disqualifications => profile
            .disqualifications
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.officer_name.clone(),
                    vec![or_missing(row.case_number.as_ref())],
                    row.provenance.clone(),
                )
                .with_pivot(row.pivot.clone())
                .with_temporal(row.temporal.clone())
            })
            .collect(),
        SectionId::Related => profile
            .related
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.name.clone(),
                    vec![
                        status_label(&row.status),
                        format!(
                            "{}: {}",
                            row.shared_connection.kind, row.shared_connection.value
                        ),
                    ],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(std::slice::from_ref(&row.ch_number)))
                .with_pivot(row.pivot.clone())
            })
            .collect(),
        SectionId::TrusteeOf => linked_rows(&profile.trustee_of.rows),
        SectionId::DonatesTo => linked_rows(&profile.donates_to.rows),
        SectionId::OffshoreLinks => linked_rows(&profile.offshore_links.rows),
        SectionId::Sanctions => profile
            .sanctions
            .iter()
            .map(|row| {
                // `on_list: false` is a real answer — "checked, no match" — and is why the whole
                // struct being absent means something else entirely: no check is represented.
                let result = if row.on_list {
                    "On a sanctions list".to_string()
                } else {
                    "Checked, no match".to_string()
                };
                RowView::new(
                    row.dataset
                        .clone()
                        .unwrap_or_else(|| "Sanctions screening".to_string()),
                    vec![result],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(&[
                    row.match_name.clone(),
                    row.match_score.map(format_confidence),
                ]))
            })
            .collect(),
        SectionId::LpaPresence => profile
            .lpa_presence
            .iter()
            .map(|row| RowView {
                primary: row.lpa_name.clone().unwrap_or_else(|| row.lpa_code.clone()),
                detail: detail_line(&[Some(row.lpa_code.clone())]),
                pivot: None,
                cells: vec![
                    row.contract_count.to_string(),
                    row.planning_application_count.to_string(),
                ],
                // Deliberately none — see `RowView::provenance`.
                provenance: None,
                temporal: None,
                verdict: None,
            })
            .collect(),
        SectionId::Findings => profile
            .findings
            .findings
            .rows
            .iter()
            .map(|row| {
                let mut view = RowView::new(
                    row.probe.clone(),
                    vec![row.summary.clone(), row.matched_via.clone()],
                    row.provenance.clone(),
                );
                view.verdict = Some(row.verdict.clone());
                view
            })
            .collect(),
        SectionId::Alerts => profile
            .findings
            .alerts
            .rows
            .iter()
            .map(|row| {
                // A withdrawn alert is shown, marked. Detection never retracts what it stops
                // finding, and an alert rendered as live after an operator retracted it is
                // T-23-FalseAccusation with a nicer stylesheet.
                let withdrawal =
                    row.withdrawn_at
                        .as_ref()
                        .map(|at| match row.withdrawn_reason.as_ref() {
                            Some(reason) => format!("withdrawn {}: {reason}", iso_date(at)),
                            None => format!("withdrawn {}", iso_date(at)),
                        });
                RowView::new(
                    row.motif_type.clone(),
                    vec![
                        format_confidence(row.score),
                        iso_date(&row.detected_at).to_string(),
                    ],
                    row.provenance.clone(),
                )
                .with_detail(detail_line(&[
                    Some(row.id.clone()),
                    row.ward_code.clone(),
                    withdrawal,
                ]))
            })
            .collect(),
        SectionId::OsintAlerts => profile
            .findings
            .osint_alerts
            .rows
            .iter()
            .map(|row| {
                RowView::new(
                    row.motif_type.clone(),
                    vec![row.severity.clone()],
                    row.provenance.clone(),
                )
            })
            .collect(),
    }
}

fn linked_rows(rows: &[LinkedRecordRowView]) -> Vec<RowView> {
    rows.iter()
        .map(|row| {
            RowView::new(row.label.clone(), Vec::new(), row.provenance.clone())
                .with_pivot(row.pivot.clone())
                .with_temporal(row.temporal.clone())
        })
        .collect()
}

/// How many rows of this section are on screen right now — the windowed count, which is what the
/// paging caption's `{n}` is.
pub fn shown_count(profile: &EntityProfileDto, section: SectionId, density: Density) -> usize {
    let rows = section_rows(profile, section).len();
    match density {
        Density::Full => rows,
        Density::Compact => rows.min(COMPACT_ROWS_PER_SECTION),
    }
}

/// "Showing {n} of {total}." — the phase 45 UI spec's paging caption, verbatim.
pub fn paging_caption(shown: usize, total: u64) -> String {
    format!("Showing {shown} of {total}.")
}

/// Whether a **Show more** control belongs beneath this section: rows exist beyond the window, and
/// the read path issued a cursor to fetch them with. Both, not either — a `total` larger than the
/// window with no cursor is the last page of a section whose total counts something the window
/// cannot reach, and offering a control that cannot advance is worse than offering none.
pub fn has_more<T>(page: &Page<T>, shown: usize) -> bool {
    page.next_cursor.is_some() && (shown as u64) < page.total
}

/// The `(cursor, total)` pair of a paged section, or `None` for the three sections that are not a
/// `Page` at all.
pub fn section_cursor_and_total(
    profile: &EntityProfileDto,
    section: SectionId,
) -> Option<(Option<String>, u64)> {
    let pair = match section {
        SectionId::Identifiers | SectionId::Sanctions | SectionId::LpaPresence => return None,
        SectionId::Officers => (&profile.officers.next_cursor, profile.officers.total),
        SectionId::Addresses => (&profile.addresses.next_cursor, profile.addresses.total),
        SectionId::Contracts => (&profile.contracts.next_cursor, profile.contracts.total),
        SectionId::PlanningApplications => (
            &profile.planning_applications.next_cursor,
            profile.planning_applications.total,
        ),
        SectionId::Property => (&profile.property.next_cursor, profile.property.total),
        SectionId::OsintLinks => (&profile.osint_links.next_cursor, profile.osint_links.total),
        SectionId::BeneficialOwners => (
            &profile.beneficial_owners.next_cursor,
            profile.beneficial_owners.total,
        ),
        SectionId::Disqualifications => (
            &profile.disqualifications.next_cursor,
            profile.disqualifications.total,
        ),
        SectionId::Related => (&profile.related.next_cursor, profile.related.total),
        SectionId::TrusteeOf => (&profile.trustee_of.next_cursor, profile.trustee_of.total),
        SectionId::DonatesTo => (&profile.donates_to.next_cursor, profile.donates_to.total),
        SectionId::OffshoreLinks => (
            &profile.offshore_links.next_cursor,
            profile.offshore_links.total,
        ),
        SectionId::Findings => (
            &profile.findings.findings.next_cursor,
            profile.findings.findings.total,
        ),
        SectionId::Alerts => (
            &profile.findings.alerts.next_cursor,
            profile.findings.alerts.total,
        ),
        SectionId::OsintAlerts => (
            &profile.findings.osint_alerts.next_cursor,
            profile.findings.osint_alerts.total,
        ),
    };
    Some((pair.0.clone(), pair.1))
}

/// The phase 45 UI spec Copy Deck, "Paging caption" — the control beside it.
pub const SHOW_MORE: &str = "Show more";

// ── Empty states ────────────────────────────────────────────────────────────────────────────────
//
// **The load-bearing contract of this screen** (the phase 45 UI spec, PIV-02's third must-have). Three
// emptinesses mean three different things, and one "No results" for all of them renders a wrong
// answer as a blank:
//
// 1. the entity genuinely has no such facts;
// 2. the section is paged and more rows exist beyond this window — which is not an empty state at
//    all, and the thing that makes it distinguishable is that it never renders one;
// 3. **no probe was capable of naming this entity**, so the findings section is empty for a reason
//    about the evidence model rather than about the entity.
//
// The third is measured, not hypothetical. The plan 44-21 summary found twelve of thirteen candidate
// probes carry no entity id at all — their evidence text interpolates counts, producer names or
// adapter names, never an id — so for most entities this section is empty, and an unqualified
// "No findings" reads as "this entity is clean" when the truth is "nothing was capable of looking".
// The two numbers come from `FindingsSectionView`, which makes them mandatory non-`Option` fields
// precisely so a renderer cannot show this empty state without them.

/// Why a section has no rows on screen.
///
/// There is no variant for "an as-at filter excluded them", and that absence is deliberate rather
/// than an oversight — see [`as_at_notice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyReason {
    /// The entity has no such facts. The plain answer, and the only one that is about the entity.
    HasNone,
    /// Probes that can name an entity contributed findings, and none of them named this one
    /// (`FindingsOutcomeView::NotNamed`). Also about the entity — but about the *probes'* answer,
    /// not about whether it has facts.
    NotNamedByAnyProbe,
    /// No probe capable of naming an entity contributed a finding at all
    /// (`FindingsOutcomeView::NotSearched`). **Not about the entity**: this empty list is evidence
    /// about the evidence model.
    NoProbeCouldName { unable: u32, probes: u32 },
}

/// What a section's body is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionState {
    /// Rows to render, and the two numbers the paging caption is made of. `shown < total` is the
    /// paged case, which is never empty and never renders an empty state.
    Rows {
        shown: usize,
        total: u64,
    },
    Empty(EmptyReason),
}

/// Classifies one section as the renderer will draw it.
///
/// The findings section is the only one whose emptiness needs more than the row count, and it is
/// the only one given more: `FindingsOutcomeView` is read for it and for nothing else, so a
/// mutation that ignored the outcome would collapse exactly the distinction D-05 exists to keep.
pub fn section_state(
    profile: &EntityProfileDto,
    section: SectionId,
    density: Density,
) -> SectionState {
    let shown = shown_count(profile, section, density);
    let total = section_total(profile, section);
    if shown > 0 {
        return SectionState::Rows { shown, total };
    }
    if section == SectionId::Findings {
        let findings = &profile.findings;
        return SectionState::Empty(match findings.outcome {
            FindingsOutcomeView::NotSearched => EmptyReason::NoProbeCouldName {
                unable: findings.probes_unable_to_name_an_entity,
                probes: findings.probes_total,
            },
            FindingsOutcomeView::Named | FindingsOutcomeView::NotNamed => {
                EmptyReason::NotNamedByAnyProbe
            }
        });
    }
    SectionState::Empty(EmptyReason::HasNone)
}

/// The section's name as it reads inside a sentence: lower-cased, except a glossary token, which
/// is a proper abbreviation and keeps its case.
pub fn section_sentence_parts(section: SectionId) -> (String, Option<&'static str>, String) {
    let (prefix, token, suffix) = section_label_parts(section);
    (prefix.to_lowercase(), token, suffix.to_lowercase())
}

/// [`section_sentence_parts`] as one string.
pub fn section_sentence_noun(section: SectionId) -> String {
    let (prefix, token, suffix) = section_sentence_parts(section);
    format!("{prefix}{}{suffix}", token.unwrap_or_default())
}

/// The sentence an empty section renders. Three reasons, three sentences, no shared fallback.
///
/// The `NoProbeCouldName` wording is the phase 45 UI spec's copy deck verbatim, and its two numbers are
/// read from the wire — `probes_unable_to_name_an_entity` and `probes_total`, computed from the
/// probe battery by plan 45-05 — never typed in here. A hard-coded "12 of 13" would be right today
/// and silently wrong the first time a probe learns to emit a `RecordRef`.
pub fn empty_state_sentence(section: SectionId, reason: &EmptyReason) -> Sentence {
    match reason {
        EmptyReason::HasNone => {
            let (prefix, token, suffix) = section_sentence_parts(section);
            Sentence {
                before: format!("No {prefix}"),
                token,
                after: format!("{suffix} recorded for this entity."),
            }
        }
        EmptyReason::NotNamedByAnyProbe => Sentence::text(
            "No probe finding names this entity. Probes that can name one did contribute \
             findings, so this is an answer about the entity.",
        ),
        EmptyReason::NoProbeCouldName { unable, probes } => Sentence::text(format!(
            "No probe finding names this entity. {unable} of {probes} probes report only aggregate \
             counts and cannot name an entity at all."
        )),
    }
}

/// A sentence that may contain one glossary abbreviation, kept in three pieces.
///
/// D-03/NAV-04 require a rendered glossary token to sit inside `<abbr>`, and the profile has two
/// sentences that name one: the OSINT sections' empty state and their as-at notice. Returning the
/// sentence whole would force a choice between a bare token on screen and a second copy of the
/// wording inside a component — so it is returned in parts, and [`Sentence::plain`] reassembles it
/// for the tests that pin the wording. One source, one tested string, and the token still renders
/// through `<Abbr>`.
///
/// This is not a way around the `<abbr>` sweep. The sweep found this module's first attempt —
/// `("OSINT link", "OSINT links")` written as literals in `section_row_noun` — and those four
/// offenders are why this type exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence {
    pub before: String,
    pub token: Option<&'static str>,
    pub after: String,
}

impl Sentence {
    /// A sentence with no abbreviation in it.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            before: text.into(),
            token: None,
            after: String::new(),
        }
    }

    /// The whole sentence as one string — what a test pins and what a screen reader would hear.
    pub fn plain(&self) -> String {
        format!(
            "{}{}{}",
            self.before,
            self.token.unwrap_or_default(),
            self.after
        )
    }
}

/// The section's **rows** as a noun, singular or plural, in the same three pieces
/// [`section_label_parts`] uses.
///
/// The phase 45 UI spec's checker asked for exactly this ("declare a lowercase singular/plural display
/// form per section and a singular verb form") after finding the spec's own copy misreading at
/// n=1: `1 Contracts exist but fall outside the "as at" date.` English does not derive
/// "planning application" or "related entity" from a suffix rule, so the pairs are written down.
///
/// Distinct from [`section_sentence_noun`], which names the *section* ("no contracts recorded")
/// rather than its rows ("3 contracts fall outside").
pub fn section_row_noun_parts(
    section: SectionId,
    count: usize,
) -> (&'static str, Option<&'static str>, &'static str) {
    let (one, many) = match section {
        SectionId::Identifiers => (("identifier", None, ""), ("identifiers", None, "")),
        SectionId::Officers => (("officer", None, ""), ("officers", None, "")),
        SectionId::Addresses => (("address", None, ""), ("addresses", None, "")),
        SectionId::Contracts => (("contract", None, ""), ("contracts", None, "")),
        SectionId::PlanningApplications => (
            ("planning application", None, ""),
            ("planning applications", None, ""),
        ),
        SectionId::Property => (
            ("property record", None, ""),
            ("property records", None, ""),
        ),
        SectionId::OsintLinks => (
            ("", Some(OSINT_ABBR), " link"),
            ("", Some(OSINT_ABBR), " links"),
        ),
        SectionId::BeneficialOwners => (
            ("beneficial owner", None, ""),
            ("beneficial owners", None, ""),
        ),
        SectionId::Disqualifications => (
            ("disqualification", None, ""),
            ("disqualifications", None, ""),
        ),
        SectionId::Related => (("related entity", None, ""), ("related entities", None, "")),
        SectionId::TrusteeOf => (("trusteeship", None, ""), ("trusteeships", None, "")),
        SectionId::DonatesTo => (
            ("donation record", None, ""),
            ("donation records", None, ""),
        ),
        SectionId::OffshoreLinks => (("offshore link", None, ""), ("offshore links", None, "")),
        SectionId::Sanctions => (
            ("sanctions check", None, ""),
            ("sanctions checks", None, ""),
        ),
        SectionId::LpaPresence => (("authority", None, ""), ("authorities", None, "")),
        SectionId::Findings => (("probe finding", None, ""), ("probe findings", None, "")),
        SectionId::Alerts => (("motif alert", None, ""), ("motif alerts", None, "")),
        SectionId::OsintAlerts => (
            ("", Some(OSINT_ABBR), " alert"),
            ("", Some(OSINT_ABBR), " alerts"),
        ),
    };
    if count == 1 {
        one
    } else {
        many
    }
}

/// [`section_row_noun_parts`] as one string, for a test to pin and for a caller with nowhere to
/// render an `<abbr>`.
pub fn section_row_noun(section: SectionId, count: usize) -> String {
    let (prefix, token, suffix) = section_row_noun_parts(section, count);
    format!("{prefix}{}{suffix}", token.unwrap_or_default())
}

/// The notice a section renders when an as-at date is set and some of its rows fall outside it.
///
/// **The phase 45 UI spec lists this as a fourth empty state, and as an empty state it is unreachable.**
/// The spec's own as-at contract says a row excluded by the filter "is not silently dropped", 45-06
/// settled that an as-at filter *marks* rows and removes none, and PIV-04's worked example requires
/// an out-of-force row to be "visually distinguished from one that does" span the date — which it
/// cannot be if it is not on screen. All three say the same thing: the rows are still here, so the
/// section is not empty.
///
/// The spec's sentence is therefore rendered where it is true — as a notice above rows that are
/// present and marked — rather than as an empty state that could only appear if the renderer first
/// broke the rule the rest of the contract depends on. Reported as a deviation rather than resolved
/// silently.
pub fn as_at_notice(rows: &[RowView], section: SectionId, as_at: Option<&str>) -> Option<Sentence> {
    as_at?;
    let outside = rows
        .iter()
        .filter(|row| {
            row.temporal
                .as_ref()
                .is_some_and(|t| t.status == AsAtStatus::OutOfForce)
        })
        .count();
    if outside == 0 {
        return None;
    }
    let verb = if outside == 1 { "falls" } else { "fall" };
    let (prefix, token, suffix) = section_row_noun_parts(section, outside);
    Some(Sentence {
        before: format!("{outside} {prefix}"),
        token,
        after: format!("{suffix} {verb} outside the \"as at\" date — kept and marked below."),
    })
}

/// The chip one row carries under an active as-at filter, or `None` when it carries none.
///
/// A plain function rather than a branch inside the component, so the three-way outcome is
/// host-testable: the two marks are different answers and an unmarked row is a third.
/// `NotFiltered` and `InForce` are both unmarked and for different reasons — one was never tested,
/// the other was tested and passed — but neither needs a chip, because the *absence* of a mark
/// beside a visible range says everything a chip would.
pub fn row_as_at_mark(row: &RowView) -> Option<&'static str> {
    match row.temporal.as_ref()?.status {
        AsAtStatus::OutOfForce => Some(OUTSIDE_AS_AT),
        AsAtStatus::Undated => Some(NOT_TESTED),
        AsAtStatus::InForce | AsAtStatus::NotFiltered => None,
    }
}

/// The record fields a range was read from, whichever arm it is.
///
/// 45-06 put a `basis` on **both** arms so a reader can tell "this dataset records no dates" from
/// "this row lost its dates". Nothing rendered it until this plan.
pub fn range_basis(range: &EffectiveRange) -> &str {
    match range {
        EffectiveRange::Recorded { basis, .. } | EffectiveRange::Unrecorded { basis } => basis,
    }
}

/// The notice a section renders when an as-at date is set and some of its rows **could not be
/// tested against it**.
///
/// [`as_at_notice`] is about rows the filter placed *outside* the date. This is about rows it could
/// not place at all, and the two are different answers about different populations — collapsing
/// them would let an analyst read "nothing fell outside" as "everything was in force".
///
/// It is the half of PIV-04 that decides whether the feature is honest. 45-06 measured entire
/// sections of this corpus as undated, so the ordinary live outcome of setting a date is a list
/// that does not change and a filter that vouched for nothing — which, unsaid, is indistinguishable
/// from a filter confirming every row. The count is stated against the section's own population
/// (`{n} of {total}`) so the reader can see how much of the section the filter reached.
///
/// The `basis` clause is the read path's own field names, and it appears only when every untested
/// row in the section shares one: "this dataset records no dates" and "this row lost its dates" are
/// different facts, and the first is what a single shared basis means.
pub fn as_at_untested_notice(
    rows: &[RowView],
    section: SectionId,
    as_at: Option<&str>,
) -> Option<Sentence> {
    let as_at = as_at.filter(|d| !d.is_empty())?;
    let untested: Vec<&RowView> = rows
        .iter()
        .filter(|row| {
            row.temporal
                .as_ref()
                .is_some_and(|t| t.status == AsAtStatus::Undated)
        })
        .collect();
    if untested.is_empty() {
        return None;
    }
    let n = untested.len();
    let total = rows.len();
    let verb = if n == 1 { "records" } else { "record" };
    let (prefix, token, suffix) = section_row_noun_parts(section, n);

    let mut bases: Vec<&str> = untested
        .iter()
        .filter_map(|row| row.temporal.as_ref())
        .map(|t| range_basis(&t.range))
        .collect();
    bases.sort_unstable();
    bases.dedup();
    let shared = match bases.as_slice() {
        [one] if !one.is_empty() => format!(" Fields consulted: {one}."),
        _ => String::new(),
    };

    Some(Sentence {
        before: format!("{n} {prefix}"),
        token,
        after: format!(
            "{suffix} of {total} {verb} no date range and {} not tested against {as_at} — kept, \
             and marked \"{NOT_TESTED}\" below.{shared}",
            if n == 1 { "was" } else { "were" }
        ),
    })
}

/// One sentence, with its glossary abbreviation inside `<Abbr>` (D-03/NAV-04). See [`Sentence`].
#[component]
#[allow(non_snake_case)]
fn SentenceView(sentence: Sentence) -> impl IntoView {
    view! {
        <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
            {sentence.before}
            {sentence.token.map(|t| view! { <Abbr token=t.to_string() /> })}
            {sentence.after}
        </p>
    }
}

/// Whether this section shows a per-row provenance chip.
///
/// Two reasons it does not, and they are different: `Compact` shows none at all (at 280px a chip
/// would dominate the row, so the panel reports one profile-level count instead), and the LPA
/// presence section has no per-row provenance to show — its rows are counts spanning two datasets,
/// which the section marker says in its own words rather than leaving a column of blanks.
pub fn renders_provenance_column(section: SectionId, density: Density) -> bool {
    density == Density::Full && section != SectionId::LpaPresence
}

/// One section: its header, its total, its rows and its paging caption.
#[component]
#[allow(non_snake_case)]
pub fn ProfileSection(
    section: SectionId,
    profile: EntityProfileDto,
    density: Density,
    /// A required prop that may be `None` — the inspection panel's `Compact` density has no
    /// **Show more**, and a control that cannot advance must not be rendered at all.
    on_show_more: Option<Callback<(String, String)>>,
) -> impl IntoView {
    let total = section_total(&profile, section);
    let all_rows = section_rows(&profile, section);
    let shown = shown_count(&profile, section, density);
    let rows: Vec<RowView> = all_rows.into_iter().take(shown).collect();
    let columns = section_columns(section);
    let with_temporal = section_has_temporal(section);
    let with_verdict = section == SectionId::Findings;
    let as_at = profile.as_at.clone();
    let anchor = section_anchor(section);
    let caption = paging_caption(shown, total);
    let cursor_and_total = section_cursor_and_total(&profile, section);
    let more = match (&cursor_and_total, density, on_show_more.is_some()) {
        (Some((Some(cursor), section_total)), Density::Full, true) => {
            ((shown as u64) < *section_total).then(|| cursor.clone())
        }
        _ => None,
    };
    let cursor_name = section_cursor_name(section).unwrap_or_default().to_string();
    // Provenance is classified over the rows ON SCREEN, which is what the marker's own counts
    // describe. `Compact` renders no chips at all — at 280px they would dominate the row — and
    // reports one profile-level count in the header instead (the phase 45 UI spec, and 45-04's
    // measurement supports it more strongly than the spec assumed).
    let row_provenance: Vec<Option<Provenance>> =
        rows.iter().map(|row| row.provenance.clone()).collect();
    let classified = section_provenance(&row_provenance);
    let unattributed_chips = shows_unattributed_chips(&classified);
    let with_provenance = renders_provenance_column(section, density);
    let marker_profile = profile.clone();
    let state = section_state(&profile, section, density);
    let empty_sentence = match &state {
        SectionState::Empty(reason) => Some(empty_state_sentence(section, reason)),
        SectionState::Rows { .. } => None,
    };
    let notice = as_at_notice(&rows, section, as_at.as_deref());
    let untested = as_at_untested_notice(&rows, section, as_at.as_deref());

    view! {
        <section id=anchor style="display: flex; flex-direction: column; gap: var(--space-sm);">
            <header style="display: flex; align-items: baseline; gap: var(--space-sm);">
                <h2 class="text-label" style="margin: 0; color: var(--foreground);">
                    <SectionLabel section=section />
                </h2>
                <span class="text-label font-mono" style="color: var(--muted-foreground);">
                    {total.to_string()}
                </span>
            </header>
            {(density == Density::Full).then(|| view! {
                <SectionProvenanceMarker section=section profile=marker_profile />
            })}
            {notice.map(|sentence| view! { <SentenceView sentence=sentence /> })}
            {untested.map(|sentence| view! { <SentenceView sentence=sentence /> })}
            {empty_sentence.map(|sentence| view! { <SentenceView sentence=sentence /> })}
            {(!rows.is_empty()).then(|| {
                let rows = rows.clone();
                let as_at = as_at.clone();
                view! {
                    <div style="overflow-x: auto;">
                        <table class="text-body" style="width: 100%; border-collapse: collapse;">
                            <thead>
                                <tr>
                                    {columns.iter().map(|c| view! {
                                        <th class="text-label" style="text-align: left; padding: var(--space-sm); color: var(--muted-foreground); font-weight: 600;">{*c}</th>
                                    }).collect_view()}
                                    {with_verdict.then(|| view! {
                                        <th class="text-label" style="text-align: left; padding: var(--space-sm); color: var(--muted-foreground); font-weight: 600;">"Verdict"</th>
                                    })}
                                    {with_temporal.then(|| view! {
                                        <th class="text-label" style="text-align: left; padding: var(--space-sm); color: var(--muted-foreground); font-weight: 600;">{RANGE_COLUMN}</th>
                                    })}
                                    {with_provenance.then(|| view! {
                                        <th class="text-label" style="text-align: left; padding: var(--space-sm); color: var(--muted-foreground); font-weight: 600;">"Source"</th>
                                    })}
                                </tr>
                            </thead>
                            <tbody>
                                {rows.into_iter().map(|row| {
                                    let as_at = as_at.clone();
                                    view! {
                                        <ProfileRow
                                            row=row
                                            as_at=as_at
                                            with_temporal=with_temporal
                                            with_verdict=with_verdict
                                            with_provenance=with_provenance
                                            unattributed_chips=unattributed_chips
                                        />
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                    <span class="text-label" style="color: var(--muted-foreground);">{caption.clone()}</span>
                }
            })}
            {more.map(|cursor| {
                let cursor_name = cursor_name.clone();
                view! {
                    <button
                        type="button"
                        style="align-self: flex-start; min-height: 44px; padding: 0 var(--space-md); background: transparent; border: 1px solid var(--border); border-radius: 6px; color: var(--primary); cursor: pointer;"
                        on:click=move |_| {
                            if let Some(callback) = on_show_more {
                                callback.run((cursor_name.clone(), cursor.clone()));
                            }
                        }
                    >
                        {SHOW_MORE}
                    </button>
                }
            })}
        </section>
    }
}

/// One row. The primary cell is an anchor when, and only when, the target's node class has a
/// profile route (`crate::profile::pivot::pivot_href`) — otherwise it is plain text, never a dead link.
#[component]
#[allow(non_snake_case)]
fn ProfileRow(
    row: RowView,
    as_at: Option<String>,
    with_temporal: bool,
    with_verdict: bool,
    with_provenance: bool,
    unattributed_chips: bool,
) -> impl IntoView {
    let href = pivot_href(row.pivot.as_ref(), as_at.as_deref());
    let out_of_force = row
        .temporal
        .as_ref()
        .is_some_and(|t| t.status == AsAtStatus::OutOfForce);
    // Only an out-of-force row is muted; an untested one is chipped at full contrast. See
    // `NOT_TESTED`.
    let mark = row_as_at_mark(&row);
    let cell_style = if out_of_force {
        "padding: var(--space-sm); vertical-align: top; border-top: 1px solid var(--border); color: var(--muted-foreground);"
    } else {
        "padding: var(--space-sm); vertical-align: top; border-top: 1px solid var(--border);"
    };
    let primary = row.primary.clone();
    let detail = row.detail.clone();
    let range = row.temporal.as_ref().map(range_text);
    let provenance = row.provenance.clone();

    view! {
        <tr style="min-height: 44px;">
            <td style=cell_style>
                <span style="display: flex; flex-direction: column; gap: var(--space-xs);">
                    {match href {
                        Some(href) => view! {
                            <a href=href style="color: var(--primary); text-decoration: underline; text-underline-offset: var(--space-xs);">{primary}</a>
                        }.into_any(),
                        None => view! { <span>{primary}</span> }.into_any(),
                    }}
                    {detail.map(|d| view! {
                        <span class="text-label font-mono" style="color: var(--muted-foreground); overflow-wrap: anywhere;">{d}</span>
                    })}
                </span>
            </td>
            {row.cells.into_iter().map(|cell| view! {
                <td style=cell_style>{cell}</td>
            }).collect_view()}
            {with_verdict.then(|| {
                let verdict = row.verdict.clone();
                view! {
                    <td style=cell_style>
                        {verdict.map(|v| view! { <VerdictBadge verdict=v /> })}
                    </td>
                }
            })}
            {with_temporal.then(|| view! {
                <td style=cell_style>
                    <span style="display: flex; flex-direction: column; gap: var(--space-xs);">
                        <span class="font-mono text-label">{range.clone().unwrap_or_default()}</span>
                        {mark.map(|text| view! {
                            <span class="text-label" style="color: var(--muted-foreground); border: 1px solid var(--border); border-radius: 4px; padding: 0 var(--space-xs); align-self: flex-start;">{text}</span>
                        })}
                    </span>
                </td>
            })}
            {with_provenance.then(|| view! {
                <td style=cell_style>
                    {provenance.clone().map(|p| view! {
                        <ProvenanceChip provenance=p show_unattributed=unattributed_chips />
                    })}
                </td>
            })}
        </tr>
    }
}

/// Every section, in [`SECTION_ORDER`].
#[component]
#[allow(non_snake_case)]
pub fn ProfileSections(
    profile: EntityProfileDto,
    density: Density,
    /// Required, and may be `None` — see [`ProfileSection`]'s prop of the same name.
    on_show_more: Option<Callback<(String, String)>>,
) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-md);">
            {SECTION_ORDER.iter().map(|section| {
                let profile = profile.clone();
                view! {
                    <ProfileSection
                        section=*section
                        profile=profile
                        density=density
                        on_show_more=on_show_more
                    />
                }
            }).collect_view()}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::test_fixtures::{
        dated, empty_profile, in_force, out_of_force, page, sample_profile, unattributed, undated,
        untested, DIRECTOR_NAMESPACE_REASON,
    };
    use urban_integrity_workbench_dto::entity_profile::OfficerRowView;

    #[test]
    fn the_spec_s_eight_sections_keep_their_fixed_order_and_findings_stays_last() {
        let order: Vec<SectionId> = SECTION_ORDER.to_vec();
        let specified = [
            SectionId::Identifiers,
            SectionId::Officers,
            SectionId::Addresses,
            SectionId::Contracts,
            SectionId::PlanningApplications,
            SectionId::Property,
            SectionId::OsintLinks,
        ];
        let positions: Vec<usize> = specified
            .iter()
            .map(|s| {
                order
                    .iter()
                    .position(|o| o == s)
                    .expect("section is ordered")
            })
            .collect();
        assert_eq!(
            positions,
            (0..specified.len()).collect::<Vec<_>>(),
            "the phase 45 UI spec fixes the first seven positions"
        );
        assert_eq!(
            &order[order.len() - 3..],
            &[
                SectionId::Findings,
                SectionId::Alerts,
                SectionId::OsintAlerts
            ],
            "the three findings sections are last because they are about the sections above them"
        );
    }

    /// Every section the DTO carries reaches the screen. The count is asserted as a number as well
    /// as by presence, so a variant added to `SectionId` without a place in `SECTION_ORDER` is
    /// caught rather than silently dropped.
    #[test]
    fn every_section_is_ordered_exactly_once() {
        assert_eq!(SECTION_ORDER.len(), 18);
        let mut seen: Vec<SectionId> = SECTION_ORDER.to_vec();
        seen.sort_by_key(|s| format!("{s:?}"));
        seen.dedup();
        assert_eq!(seen.len(), SECTION_ORDER.len(), "a section is listed twice");
    }

    #[test]
    fn a_section_name_carrying_a_glossary_token_keeps_it_separable() {
        assert_eq!(section_noun(SectionId::OsintLinks), "OSINT links");
        assert_eq!(
            section_label_parts(SectionId::OsintLinks),
            ("", Some("OSINT"), " links")
        );
        assert_eq!(section_noun(SectionId::LpaPresence), "LPA presence");
        // Non-vacuity: a section with no token must produce no token, or "every name has parts"
        // would be indistinguishable from "every name is one piece".
        assert_eq!(
            section_label_parts(SectionId::Contracts),
            ("Contracts", None, "")
        );
    }

    #[test]
    fn every_paged_section_names_a_section_the_handler_accepts() {
        // `crates/workbench`'s `profile_map::SECTIONS`, verbatim. A name this list gets wrong is a
        // 400 on the first Show more press, which no type would catch.
        const HANDLER_SECTIONS: &[&str] = &[
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
        let mut paged = 0;
        for section in SECTION_ORDER {
            if let Some(name) = section_cursor_name(*section) {
                paged += 1;
                assert!(
                    HANDLER_SECTIONS.contains(&name),
                    "{name} is not a section the profile handler pages"
                );
            }
        }
        assert_eq!(paged, 15, "fifteen sections are paged; three are not");
    }

    #[test]
    fn the_index_lists_every_section_with_its_unwindowed_total() {
        let profile = sample_profile();
        let entries = section_index_entries(&profile);
        assert_eq!(entries.len(), SECTION_ORDER.len());
        let officers = entries
            .iter()
            .find(|(s, _)| *s == SectionId::Officers)
            .expect("officers is indexed");
        // The fixture's officers page holds one row of a 2,400-row total: the index states the
        // total, never the window.
        assert_eq!(officers.1, 2_400);
        assert_eq!(profile.officers.rows.len(), 1);
    }

    #[test]
    fn an_empty_section_is_still_indexed_at_zero() {
        let profile = empty_profile();
        let entries = section_index_entries(&profile);
        assert_eq!(entries.len(), SECTION_ORDER.len());
        for (section, total) in &entries {
            let expected = u64::from(*section == SectionId::Identifiers);
            assert_eq!(
                *total, expected,
                "{section:?} of an empty profile must index at {expected} — identifiers is the \
                 one section that is never empty, because an entity always has a status"
            );
        }
    }

    /// The pair `section_columns`/`section_rows` is the only per-section code in this module, so
    /// the one thing that can silently go wrong between them is a column count. Asserted for every
    /// section, over a fixture that populates every section, with the row count checked first so a
    /// section that produced nothing cannot make its own assertion vacuous.
    #[test]
    fn every_row_has_one_cell_per_column() {
        let profile = sample_profile();
        let mut checked = 0;
        for section in SECTION_ORDER {
            let rows = section_rows(&profile, *section);
            assert!(
                !rows.is_empty(),
                "{section:?} produced no rows — the fixture must populate every section or this \
                 sweep proves nothing"
            );
            let columns = section_columns(*section).len();
            for row in rows {
                assert_eq!(
                    row.cells.len() + 1,
                    columns,
                    "{section:?} row {:?} has {} cells for {columns} columns",
                    row.primary,
                    row.cells.len()
                );
                checked += 1;
            }
        }
        assert!(checked >= SECTION_ORDER.len(), "every section was checked");
    }

    /// The identifiers section leads with the status claim, and that claim carries its own
    /// provenance — 45-04 measured the header as 88.9% unattributed, so a status rendered as a bare
    /// fact would be the most confident-looking unsourced claim on the page.
    #[test]
    fn the_identifiers_section_leads_with_the_status_and_its_provenance() {
        let rows = section_rows(&sample_profile(), SectionId::Identifiers);
        assert_eq!(rows[0].primary, "Status");
        assert_eq!(rows[0].cells, vec!["Dissolved 2019-04-02".to_string()]);
        assert!(rows[0].provenance.is_some());
        assert_eq!(rows[1].primary, "Companies House number");
    }

    /// A contract row keeps every fact the wire carries: the money in the cells, and the tender
    /// reference and ward code in the row's mono detail line rather than dropped for width.
    #[test]
    fn a_contract_row_keeps_its_money_its_tender_reference_and_its_ward() {
        let rows = section_rows(&sample_profile(), SectionId::Contracts);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "£125,000");
        let detail = rows[0].detail.as_deref().expect("a tender ref exists");
        assert!(detail.contains("tender-0001"), "detail was {detail}");
        assert!(detail.contains("X05999001"), "detail was {detail}");
    }

    #[test]
    fn money_is_grouped_and_never_rounded_away() {
        assert_eq!(format_money(125_000.0), "£125,000");
        assert_eq!(format_money(1_250.5), "£1,250.50");
        assert_eq!(format_money(0.0), "£0");
        assert_eq!(format_money(1_234_567.89), "£1,234,567.89");
    }

    /// PROVE THIS CAN FAIL: return `"present"` for an open end and this fails — which is the point.
    /// `effective_to` is null on every edge in this store, so "ongoing" would be a claim no dataset
    /// made, on every row.
    #[test]
    fn an_open_ended_range_is_not_reported_as_ongoing() {
        let temporal = dated(Some("2024-06-11"), None, "contract.date_published");
        let text = range_text(&temporal);
        assert_eq!(text, "2024-06-11 →");
        for forbidden in ["present", "ongoing", "current", "now"] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "an open end must not be worded as {forbidden}: {text}"
            );
        }
        assert_eq!(
            range_text(&undated("located_at.meta.effective_from")),
            "Undated"
        );
    }

    /// **The "In force" column is declared exactly where the rows carry a range**, in both
    /// directions, checked against the rows [`section_rows`] actually builds.
    ///
    /// [`section_has_temporal`] is a hand-written list, the same shape as the [`SECTION_ORDER`]
    /// list 45-10 had to correct a doc comment about. Two silent failures live in the gap, and
    /// neither raises an error: a section whose rows carry a range but is missing from the list
    /// renders **no dates at all** — the exact "the unfiltered profile is the less informative
    /// screen" defect PIV-04 forbids — and a section on the list whose rows carry none renders a
    /// column of blank cells that reads as a rendering fault.
    ///
    /// The half-and-half case is rejected too: a section where only some rows carry a range would
    /// put empty cells beside full ones, which is indistinguishable from dates that failed to load.
    ///
    /// PROVE THIS CAN FAIL: add `SectionId::Related` to `section_has_temporal` (that section's rows
    /// carry no `temporal` — 45-06 explains why), or delete `.with_temporal(row.temporal.clone())`
    /// from `linked_rows`. Either fails naming the section.
    #[test]
    fn the_in_force_column_is_declared_exactly_where_the_rows_carry_a_range() {
        let profile = sample_profile();
        let mut with_range: Vec<SectionId> = Vec::new();
        let mut without_range: Vec<SectionId> = Vec::new();

        for section in SECTION_ORDER {
            let rows = section_rows(&profile, *section);
            assert!(
                !rows.is_empty(),
                "{section:?} produced no rows — the fixture must populate every section or this \
                 sweep proves nothing"
            );
            let every = rows.iter().all(|row| row.temporal.is_some());
            let none = rows.iter().all(|row| row.temporal.is_none());
            assert!(
                every || none,
                "{section:?} carries a range on some rows and not others — the column would render \
                 blank cells beside full ones, which reads as dates that failed to load"
            );
            assert_eq!(
                every,
                section_has_temporal(*section),
                "{section:?}: section_has_temporal says {}, but its rows {} carry a range — either \
                 the ranges never render or the column renders empty",
                section_has_temporal(*section),
                if every { "do" } else { "do not" }
            );
            if every {
                with_range.push(*section);
            } else {
                without_range.push(*section);
            }
        }

        // The column's header must not assert a determination its cells do not carry. An undated
        // row renders `Undated` in this column, and "In force: Undated" contradicts the rule
        // 45-06 settled — on this corpus, on the majority of rows.
        assert_eq!(RANGE_COLUMN, "Date range");
        assert!(
            !RANGE_COLUMN.to_lowercase().contains("force"),
            "the column holds a range the record states, not a force determination: {RANGE_COLUMN}"
        );

        // Non-vacuity in both directions: the assertion above is satisfied by a renderer that had
        // stopped showing ranges everywhere, and by one that showed them everywhere.
        assert_eq!(
            with_range.len(),
            10,
            "ten sections take their rows from a relation that records dates: {with_range:?}"
        );
        assert!(
            without_range.len() >= 5,
            "and the rest genuinely record none — an alert or an identity link is an assertion \
             made at a moment, not a fact in force over an interval (45-06): {without_range:?}"
        );
    }

    /// **A range renders with no filter set, and reads the same once one is.**
    ///
    /// This is the phase 45 UI spec's "every temporal row renders its range *whether or not* a filter is
    /// set — the unfiltered profile must not be the less informative screen". A range that only
    /// appeared once a date was chosen would teach an analyst to set a filter in order to see facts
    /// the record states unconditionally, which is a worse habit than the filter itself is worth.
    ///
    /// The range is a property of the **record**; the status is what the *filter* made of it. So
    /// the same range under all four statuses must render the same text, and the two are kept
    /// apart in the rendering exactly as `Temporal` keeps them apart in the type.
    ///
    /// PROVE THIS CAN FAIL: make `range_text` return `String::new()` for
    /// `AsAtStatus::NotFiltered` — the unfiltered assertion fails on every temporal section. Make
    /// it return `"Undated"` for `AsAtStatus::Undated` regardless of the range and the
    /// status-invariance assertion fails.
    #[test]
    fn a_range_renders_unfiltered_and_reads_the_same_once_a_filter_is_set() {
        let profile = sample_profile();
        assert_eq!(profile.as_at, None, "the fixture is the unfiltered screen");

        let mut recorded = 0usize;
        let mut unrecorded = 0usize;

        for section in SECTION_ORDER.iter().filter(|s| section_has_temporal(**s)) {
            for row in section_rows(&profile, *section) {
                let temporal = row
                    .temporal
                    .expect("a declared temporal section carries a range");
                assert_eq!(
                    temporal.status,
                    AsAtStatus::NotFiltered,
                    "{section:?}: an unfiltered profile has not established that anything was in \
                     force — that distinction is why AsAtStatus has a fourth variant (45-06)"
                );

                let text = range_text(&temporal);
                assert!(
                    !text.is_empty() && text != NOT_RECORDED,
                    "{section:?} row {:?} rendered no range on the unfiltered screen: {text:?}",
                    row.primary
                );

                match &temporal.range {
                    EffectiveRange::Recorded { from, .. } => {
                        recorded += 1;
                        let from = from.as_deref().expect("this fixture records a start");
                        assert!(
                            text.starts_with(iso_date(from)),
                            "{section:?} row {:?} dropped its recorded start: {text}",
                            row.primary
                        );
                    }
                    EffectiveRange::Unrecorded { .. } => {
                        unrecorded += 1;
                        assert_eq!(text, "Undated");
                    }
                }

                for status in [
                    AsAtStatus::InForce,
                    AsAtStatus::OutOfForce,
                    AsAtStatus::Undated,
                ] {
                    let filtered = Temporal {
                        range: temporal.range.clone(),
                        status,
                    };
                    assert_eq!(
                        range_text(&filtered),
                        text,
                        "{section:?} row {:?}: the range is what the record says, and no filter \
                         changes it — {status:?} rendered it differently",
                        row.primary
                    );
                }
            }
        }

        // Non-vacuity, and the population counted on both sides before comparing: a fixture with
        // no dated rows would prove nothing about the recorded arm, and one with no undated rows
        // would prove nothing about the arm that is the overwhelming majority on this corpus
        // (45-06: every `Directs` and every `LocatedAt` edge in the store is undated).
        assert!(
            recorded >= 5,
            "the fixture must exercise the recorded arm on more than one section: {recorded}"
        );
        assert!(
            unrecorded >= 1,
            "the fixture must exercise the unrecorded arm — it is the common case live: \
             {unrecorded}"
        );
    }

    /// A row the as-at filter placed outside its date is **still rendered**, and marked. Nothing is
    /// removed from a profile by a filter (45-06); hiding the row would empty four sections on this
    /// corpus and turn a filter into a blanker.
    #[test]
    fn a_row_outside_the_as_at_date_is_kept_and_marked() {
        let mut profile = sample_profile();
        profile.as_at = Some("2023-01-01".to_string());
        profile.officers.rows[0].temporal = out_of_force(
            "2015-01-04",
            "2021-09-30",
            "officer.appointed_on / officer.resigned_on",
        );
        let rows = section_rows(&profile, SectionId::Officers);
        assert_eq!(rows.len(), 1, "the row is still here");
        let temporal = rows[0].temporal.as_ref().expect("officers carry a range");
        assert_eq!(temporal.status, AsAtStatus::OutOfForce);
        assert_eq!(range_text(temporal), "2015-01-04 → 2021-09-30");
    }

    /// **The test this plan exists for: which rows the filter tested, which it could not, and
    /// which it placed outside — by name, with the population counted on both sides.**
    ///
    /// A test asserting "the filtered list is shorter" passes for a filter that drops the wrong
    /// rows. Nothing is dropped here at all (45-06), so the failure it has to catch is the
    /// opposite one: a list that does not change and a screen that does not say the filter reached
    /// none of it. On this corpus that is the *ordinary* outcome — every `Directs` edge is undated
    /// — so an unsaid filter is indistinguishable from a filter that confirmed every row.
    ///
    /// PROVE THIS CAN FAIL: make `row_as_at_mark` return `None` for `AsAtStatus::Undated` (the two
    /// untested rows lose their chip), or make `as_at_untested_notice` return `None` (the section
    /// stops counting them). Either fails.
    #[test]
    fn a_row_the_filter_could_not_test_is_named_marked_and_counted() {
        const AS_AT: &str = "2021-06-30";
        const BASIS: &str = "directs.meta.effective_from / directs.meta.effective_to";
        let unfiltered = sample_profile();
        let mut profile = sample_profile();
        profile.as_at = Some(AS_AT.to_string());
        profile.officers.rows = vec![
            officer(
                "Still Serving",
                in_force("2015-01-04", None, "officer.appointed_on"),
            ),
            officer(
                "Departed Before",
                out_of_force(
                    "2015-01-04",
                    "2019-04-02",
                    "officer.appointed_on / officer.resigned_on",
                ),
            ),
            officer("No Dates One", untested(BASIS)),
            officer("No Dates Two", untested(BASIS)),
        ];
        profile.officers.total = 4;
        profile.officers.next_cursor = None;

        let rows = section_rows(&profile, SectionId::Officers);

        // **Non-vacuity, counted before anything is compared.** A fixture with no dated rows
        // proves nothing about the tested arm; one with no undated rows proves nothing about the
        // arm that is the live majority. Both must be populated for the buckets below to mean
        // anything.
        let dated_population = rows
            .iter()
            .filter(|r| {
                r.temporal
                    .as_ref()
                    .is_some_and(|t| !t.range.is_unrecorded())
            })
            .count();
        let null_population = rows.len() - dated_population;
        assert_eq!(dated_population, 2, "the bounded population must be real");
        assert_eq!(null_population, 2, "the null population must be real");

        // **Nothing left.** The population is the same on both sides of the filter — which is what
        // makes every claim below about *marking* rather than about removal.
        assert_eq!(
            rows.len(),
            section_rows(&unfiltered, SectionId::Officers).len() + 3,
            "the fixture replaced one officer with four; no row is removed by the filter itself"
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(
            section_total(&profile, SectionId::Officers),
            4,
            "the header total stays the unfiltered total (the phase 45 UI spec)"
        );

        // **Which rows, by name** — not how many. A mark applied to the wrong row passes a count.
        let marked: Vec<(&str, Option<&'static str>)> = rows
            .iter()
            .map(|r| (r.primary.as_str(), row_as_at_mark(r)))
            .collect();
        assert_eq!(
            marked,
            vec![
                ("Still Serving", None),
                ("Departed Before", Some(OUTSIDE_AS_AT)),
                ("No Dates One", Some(NOT_TESTED)),
                ("No Dates Two", Some(NOT_TESTED)),
            ],
            "each row must carry the mark for what the filter actually made of it"
        );

        // The two sentences are about two different populations and say so.
        let outside = as_at_notice(&rows, SectionId::Officers, Some(AS_AT))
            .expect("one row falls outside")
            .plain();
        assert_eq!(
            outside,
            "1 officer falls outside the \"as at\" date — kept and marked below."
        );

        let untested_notice = as_at_untested_notice(&rows, SectionId::Officers, Some(AS_AT))
            .expect("two rows could not be tested")
            .plain();
        assert_eq!(
            untested_notice,
            format!(
                "2 officers of 4 record no date range and were not tested against {AS_AT} — kept, \
                 and marked \"not tested\" below. Fields consulted: {BASIS}."
            )
        );
    }

    /// **A section the filter reached none of still says so.** The case with no out-of-force row is
    /// the one that matters live: `as_at_notice` returns `None`, the list is visibly unchanged, and
    /// without the untested notice the screen would be silent about a filter that vouched for
    /// nothing.
    ///
    /// PROVE THIS CAN FAIL: have `as_at_untested_notice` return `None` when `as_at_notice` does —
    /// the "wholly untested" assertion fails with no sentence at all.
    #[test]
    fn a_section_the_filter_could_not_test_at_all_does_not_go_silent() {
        const AS_AT: &str = "2021-06-30";
        const BASIS: &str = "located_at.meta.effective_from / located_at.meta.effective_to";
        let mut profile = sample_profile();
        profile.as_at = Some(AS_AT.to_string());
        profile.addresses.rows[0].temporal = untested(BASIS);

        let rows = section_rows(&profile, SectionId::Addresses);
        assert_eq!(rows.len(), 1, "the row is still here");

        // Non-vacuity: nothing here falls outside, so the older notice is genuinely silent and this
        // test is about the sentence that is not it.
        assert_eq!(as_at_notice(&rows, SectionId::Addresses, Some(AS_AT)), None);

        let sentence = as_at_untested_notice(&rows, SectionId::Addresses, Some(AS_AT))
            .expect("a wholly untested section must still account for itself")
            .plain();
        assert_eq!(
            sentence,
            format!(
                "1 address of 1 records no date range and was not tested against {AS_AT} — kept, \
                 and marked \"not tested\" below. Fields consulted: {BASIS}."
            ),
            "the sentence must agree in number at n=1 — the defect the phase 45 UI spec's own copy had"
        );

        // And an unfiltered profile says nothing at all: there is no rule to state and no filter to
        // report on, so a permanent notice would be a claim about a filter nobody ran.
        assert_eq!(
            as_at_untested_notice(&rows, SectionId::Addresses, None),
            None
        );
        assert_eq!(
            as_at_untested_notice(&rows, SectionId::Addresses, Some("")),
            None,
            "a bare ?as_at= is the unfiltered screen (ProfileRoute), and must read as one here too"
        );
    }

    /// The mark is the filter's answer, and the four answers are four different things. In
    /// particular an **unfiltered** row is unmarked for a different reason than an in-force one,
    /// and neither may borrow the other's chip.
    ///
    /// PROVE THIS CAN FAIL: return `Some(NOT_TESTED)` for `AsAtStatus::NotFiltered` — every row of
    /// the unfiltered profile would be chipped "not tested", which is true and useless. The first
    /// assertion fails.
    #[test]
    fn the_unfiltered_profile_carries_no_as_at_marks_at_all() {
        let profile = sample_profile();
        assert_eq!(profile.as_at, None);
        let mut temporal_rows = 0usize;
        for section in SECTION_ORDER.iter().filter(|s| section_has_temporal(**s)) {
            for row in section_rows(&profile, *section) {
                assert!(row.temporal.is_some());
                temporal_rows += 1;
                assert_eq!(
                    row_as_at_mark(&row),
                    None,
                    "{section:?} row {:?} was marked on a screen with no filter set",
                    row.primary
                );
            }
        }
        // Non-vacuity: the sweep saw rows that COULD have been marked.
        assert!(
            temporal_rows >= 10,
            "only {temporal_rows} temporal rows swept"
        );

        // And the two marks are distinct strings, so a reader can tell "outside the window you
        // asked for" from "the filter could not ask".
        assert_ne!(OUTSIDE_AS_AT, NOT_TESTED);
    }

    /// A withdrawn motif alert says so on its own row. Detection never retracts what it stops
    /// finding, so a withdrawn alert is kept — but one rendered as live is a false accusation.
    #[test]
    fn a_withdrawn_alert_says_it_was_withdrawn() {
        let mut profile = sample_profile();
        profile.findings.alerts.rows[0].withdrawn_at = Some("2026-08-20T10:00:00Z".to_string());
        profile.findings.alerts.rows[0].withdrawn_reason = Some("data correction".to_string());
        let rows = section_rows(&profile, SectionId::Alerts);
        let detail = rows[0].detail.as_deref().expect("an alert carries its id");
        assert!(
            detail.contains("withdrawn 2026-08-20: data correction"),
            "detail was {detail}"
        );
        // Non-vacuity: a live alert says nothing of the kind.
        let live = section_rows(&sample_profile(), SectionId::Alerts);
        assert!(!live[0]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("withdrawn"));
    }

    /// The caption counts what is on screen against what exists, and `Compact` counts its own
    /// truncated window rather than the full one.
    #[test]
    fn the_paging_caption_counts_the_window_against_the_total() {
        assert_eq!(paging_caption(3, 41), "Showing 3 of 41.");
        let profile = sample_profile();
        assert_eq!(shown_count(&profile, SectionId::Officers, Density::Full), 1);
        let mut many = sample_profile();
        let row = many.officers.rows[0].clone();
        many.officers.rows = vec![row; 7];
        assert_eq!(shown_count(&many, SectionId::Officers, Density::Full), 7);
        assert_eq!(
            shown_count(&many, SectionId::Officers, Density::Compact),
            COMPACT_ROWS_PER_SECTION
        );
    }

    /// A **Show more** control is offered only when there is a cursor to advance with. A total
    /// bigger than the window and no cursor is the read path saying "this is the last window";
    /// offering a button that cannot move is worse than offering none.
    #[test]
    fn show_more_needs_a_cursor_as_well_as_a_larger_total() {
        let windowed = page(vec!["a".to_string()], 40, Some("officers@20"));
        assert!(has_more(&windowed, 1));
        let last = page_without_cursor();
        assert!(!has_more(&last, 1), "no cursor means no Show more");
        let complete = page(vec!["a".to_string()], 1, Some("officers@20"));
        assert!(!has_more(&complete, 1), "nothing beyond the window");
    }

    /// One officer row with a given temporal state. Only the name and the range matter to the
    /// as-at tests, so everything else is held constant deliberately: a difference in the output
    /// can then only come from the status.
    fn officer(name: &str, temporal: Temporal) -> OfficerRowView {
        OfficerRowView {
            name: name.to_string(),
            role: Some("director".to_string()),
            appointed: None,
            resigned: None,
            pivot: None,
            provenance: unattributed(DIRECTOR_NAMESPACE_REASON),
            temporal,
        }
    }

    fn page_without_cursor() -> Page<String> {
        Page {
            rows: vec!["a".to_string()],
            next_cursor: None,
            total: 40,
        }
    }

    /// `Compact` renders no per-row chip, and the LPA presence section renders none at either
    /// density. Non-vacuity in both directions: a `Full` section that does have provenance must
    /// come back true, or "nothing has a provenance column" would pass this.
    #[test]
    fn the_provenance_column_is_full_density_only_and_never_on_an_unattributable_count() {
        assert!(renders_provenance_column(
            SectionId::Contracts,
            Density::Full
        ));
        assert!(!renders_provenance_column(
            SectionId::Contracts,
            Density::Compact
        ));
        assert!(!renders_provenance_column(
            SectionId::LpaPresence,
            Density::Full
        ));
    }

    // ── The three empty states ──────────────────────────────────────────────────────────────────
    //
    // Three tests, and each goes red for a DIFFERENT mutation. That is the property being claimed:
    // a test that only asserted "an empty state renders" would pass under all three regressions and
    // prove nothing about which answer the screen is giving.

    /// **(1) The entity has none.** The plain answer, in the section's own noun.
    ///
    /// PROVE THIS CAN FAIL: return a generic `"No results."` from `empty_state_sentence`'s
    /// `HasNone` arm — this fails, and the other two empty-state tests stay green, because neither
    /// asserts that wording.
    #[test]
    fn a_section_with_no_facts_says_the_entity_has_none_in_its_own_words() {
        let mut profile = sample_profile();
        profile.contracts = Page {
            rows: Vec::new(),
            next_cursor: None,
            total: 0,
        };
        let state = section_state(&profile, SectionId::Contracts, Density::Full);
        assert_eq!(state, SectionState::Empty(EmptyReason::HasNone));
        assert_eq!(
            empty_state_sentence(SectionId::Contracts, &EmptyReason::HasNone).plain(),
            "No contracts recorded for this entity."
        );
        // A glossary token keeps its case inside the sentence; the rest of the name does not.
        assert_eq!(
            empty_state_sentence(SectionId::OsintLinks, &EmptyReason::HasNone).plain(),
            "No OSINT links recorded for this entity."
        );
        // Non-vacuity: the same section, populated, is NOT empty — so a `section_state` that
        // returned `Empty` for everything could not pass this.
        assert_eq!(
            section_state(&sample_profile(), SectionId::Contracts, Density::Full),
            SectionState::Rows {
                shown: 1,
                total: 12
            }
        );
    }

    /// **(2) The section is paged.** It is never empty, and it says how many more there are.
    ///
    /// PROVE THIS CAN FAIL: make `paging_caption` print `"Showing {total} of {total}."` — a window
    /// reported as the whole section — and this fails while the other two stay green, neither of
    /// them asserting a caption.
    #[test]
    fn a_paged_section_is_never_empty_and_says_how_many_it_is_not_showing() {
        let mut profile = sample_profile();
        let row = profile.contracts.rows[0].clone();
        profile.contracts = Page {
            rows: vec![row; 3],
            next_cursor: Some("contracts@3".to_string()),
            total: 41,
        };
        // Non-vacuity first: this fixture really is a window onto something bigger.
        assert_eq!(profile.contracts.rows.len(), 3);
        assert_eq!(profile.contracts.total, 41);

        let state = section_state(&profile, SectionId::Contracts, Density::Full);
        assert_eq!(
            state,
            SectionState::Rows {
                shown: 3,
                total: 41
            }
        );
        let SectionState::Rows { shown, total } = state else {
            unreachable!("a paged section is never empty")
        };
        assert_eq!(paging_caption(shown, total), "Showing 3 of 41.");
        assert!(has_more(&profile.contracts, shown));
    }

    /// **(3) No probe was capable of naming this entity.** The gap is in the evidence model, not in
    /// the entity, and the sentence says so with numbers the wire supplied.
    ///
    /// The assertion is deliberately a difference rather than a wording: the two ways a findings
    /// section can be empty must not read the same, and pinning only one of the two sentences would
    /// leave the other free to drift into it.
    ///
    /// PROVE THIS CAN FAIL: make `section_state` ignore `findings.outcome` and always return
    /// `NotNamedByAnyProbe` — the two sentences become identical and this fails, while the
    /// other two empty-state tests stay green, neither of them touching the findings section.
    #[test]
    fn an_empty_findings_section_distinguishes_the_evidence_model_from_the_entity() {
        let mut not_searched = empty_profile();
        not_searched.findings.outcome = FindingsOutcomeView::NotSearched;
        let mut not_named = empty_profile();
        not_named.findings.outcome = FindingsOutcomeView::NotNamed;

        // Non-vacuity: both fixtures really do have an empty findings page, so the two sentences
        // below are two answers to the same question rather than two different questions.
        assert_eq!(not_searched.findings.findings.rows.len(), 0);
        assert_eq!(not_named.findings.findings.rows.len(), 0);

        let searched_state = section_state(&not_searched, SectionId::Findings, Density::Full);
        let named_state = section_state(&not_named, SectionId::Findings, Density::Full);
        assert_eq!(
            searched_state,
            SectionState::Empty(EmptyReason::NoProbeCouldName {
                unable: 12,
                probes: 13
            })
        );
        assert_eq!(
            named_state,
            SectionState::Empty(EmptyReason::NotNamedByAnyProbe)
        );

        let (SectionState::Empty(a), SectionState::Empty(b)) = (searched_state, named_state) else {
            unreachable!("both fixtures are empty")
        };
        let evidence_model = empty_state_sentence(SectionId::Findings, &a).plain();
        let entity = empty_state_sentence(SectionId::Findings, &b).plain();
        assert_ne!(
            evidence_model, entity,
            "the two ways a findings section can be empty must not read the same"
        );
        assert!(
            evidence_model.contains("12 of 13 probes report only aggregate counts"),
            "the counts must be rendered: {evidence_model}"
        );
        assert!(
            !entity.contains("12 of 13"),
            "the entity-side answer must not borrow the evidence-model counts: {entity}"
        );
    }

    /// The two probe counts are the wire's, never typed in. The phase 45 UI spec asks for exactly this
    /// test: "{k} and {m} are computed from the probe set by plan 45-05, not hard-coded, and a test
    /// asserts the rendered count matches it".
    #[test]
    fn the_probe_counts_in_the_empty_state_are_the_ones_the_wire_sent() {
        let mut profile = empty_profile();
        profile.findings.outcome = FindingsOutcomeView::NotSearched;
        // A different, deliberately non-live pair: a sentence built from the constants of the day
        // would still read "12 of 13" here.
        profile.findings.probes_total = 21;
        profile.findings.probes_unable_to_name_an_entity = 4;
        let SectionState::Empty(reason) =
            section_state(&profile, SectionId::Findings, Density::Full)
        else {
            unreachable!("an empty findings section")
        };
        assert!(empty_state_sentence(SectionId::Findings, &reason)
            .plain()
            .contains("4 of 21 probes report only aggregate counts"));
    }

    /// The spec's fourth empty state is a notice, because the rows it describes are still on
    /// screen. See [`as_at_notice`] for why an empty state here would be unreachable.
    #[test]
    fn rows_outside_the_as_at_date_are_announced_rather_than_hidden() {
        let mut profile = sample_profile();
        profile.as_at = Some("2023-01-01".to_string());
        profile.officers.rows[0].temporal = out_of_force(
            "2015-01-04",
            "2021-09-30",
            "officer.appointed_on / officer.resigned_on",
        );
        let rows = section_rows(&profile, SectionId::Officers);
        assert_eq!(rows.len(), 1, "the row is not hidden");
        assert_eq!(
            section_state(&profile, SectionId::Officers, Density::Full),
            SectionState::Rows {
                shown: 1,
                total: 2_400
            },
            "a section whose rows fall outside the date is not an empty section"
        );
        assert_eq!(
            as_at_notice(&rows, SectionId::Officers, Some("2023-01-01"))
                .map(|s| s.plain())
                .as_deref(),
            Some("1 officer falls outside the \"as at\" date — kept and marked below.")
        );
        // Non-vacuity in both directions: no date set, or no row outside it, and there is no
        // notice at all.
        assert_eq!(as_at_notice(&rows, SectionId::Officers, None), None);
        assert_eq!(
            as_at_notice(
                &section_rows(&sample_profile(), SectionId::Officers),
                SectionId::Officers,
                Some("2023-01-01")
            ),
            None
        );
    }

    /// The plural/singular pairs are declared, and the notice reads correctly at n=1 and n>1 —
    /// which the spec's own copy did not.
    #[test]
    fn the_as_at_notice_reads_correctly_at_one_and_at_many() {
        let mut profile = sample_profile();
        profile.as_at = Some("2023-01-01".to_string());
        let outside = out_of_force(
            "2015-01-04",
            "2021-09-30",
            "officer.appointed_on / officer.resigned_on",
        );
        let mut row = profile.officers.rows[0].clone();
        row.temporal = outside.clone();
        profile.officers.rows = vec![row.clone(), row];
        let rows = section_rows(&profile, SectionId::Officers);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            as_at_notice(&rows, SectionId::Officers, Some("2023-01-01"))
                .map(|s| s.plain())
                .as_deref(),
            Some("2 officers fall outside the \"as at\" date — kept and marked below.")
        );
        assert_eq!(
            section_row_noun(SectionId::PlanningApplications, 1),
            "planning application"
        );
        assert_eq!(section_row_noun(SectionId::Related, 2), "related entities");
    }

    /// The identifiers section can never be empty, because an entity always has a status and
    /// `StatusFactView` is not optional. Worth pinning: the empty-state sentence for that section
    /// would read "No identifiers & status recorded for this entity.", and the reason nobody has
    /// to care about that wording is a property of the wire type rather than of the copy.
    #[test]
    fn the_identifiers_section_is_never_empty_because_status_is_always_a_fact() {
        let empty = empty_profile();
        assert!(empty.identifiers.is_empty(), "no identifier rows at all");
        assert_eq!(
            section_state(&empty, SectionId::Identifiers, Density::Full),
            SectionState::Rows { shown: 1, total: 1 },
            "the status row is still there, and it is counted"
        );
    }
}
