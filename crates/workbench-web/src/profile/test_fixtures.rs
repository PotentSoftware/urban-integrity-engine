//! Profile fixtures for this module's host tests.
//!
//! Every identifier is synthetic — `X`-prefixed Companies House numbers and `X05…` ward codes —
//! per `scripts/check-no-analysis-literals.sh`, which failed plan 45-09's first commit for exactly
//! this and is the reason the rule is repeated at every fixture site rather than assumed.
//!
//! [`sample_profile`] populates every section so a sweep has something to sweep;
//! [`empty_profile`] empties all sixteen so an empty-state test cannot pass by accident on a
//! populated one. The two are built from the same constructor, which is what keeps them
//! comparable: a field added to [`EntityProfileDto`] appears in both or in neither.

use urban_integrity_workbench_dto::entity_profile::{
    AddressRowView, AlertRowView, BeneficialOwnerRowView, ContractRowView, DisqualificationRowView,
    EntityProfileDto, EntityStatusView, FindingRowView, FindingsOutcomeView, FindingsSectionView,
    IdentifierFactView, LinkedRecordRowView, LpaPresenceView, OfficerRowView, OsintAlertRowView,
    OsintLinkRowView, PlanningApplicationRowView, PropertyRowView, RelatedEntityRowView,
    SanctionsStatusView, SharedConnectionView, StatusFactView,
};
use urban_integrity_workbench_dto::verdict_view::{
    CoverageView, PopulationSourceView, VerdictView,
};
use urban_integrity_workbench_dto::{
    AsAtStatus, EffectiveRange, EntityRef, LinkTier, Page, Provenance, Temporal,
};

/// An attributed fact: source dataset, `graph_events` timestamp, no identity link.
pub fn attributed(source: &str) -> Provenance {
    Provenance::Attributed {
        source_dataset: source.to_string(),
        observed_at: "2026-07-31T00:00:00Z".to_string(),
        tier: None,
    }
}

/// A fact derived from an `identity_links` row, carrying its tier.
pub fn linked(source: &str, tier: LinkTier) -> Provenance {
    Provenance::Attributed {
        source_dataset: source.to_string(),
        observed_at: "2026-07-31T00:00:00Z".to_string(),
        tier: Some(tier),
    }
}

/// A fact whose provenance could not be established, carrying the read path's own refusal reason
/// verbatim (`crates/schema/src/types/attribution.rs`) — 45-04's instruction was "the three
/// reasons are the copy for the section marker; do not paraphrase them".
pub fn unattributed(reason: &str) -> Provenance {
    Provenance::Unattributed {
        reason: reason.to_string(),
    }
}

/// The `director:` namespace's real refusal reason, as `dataset_of_id` words it.
pub const DIRECTOR_NAMESPACE_REASON: &str =
    "the director: namespace is minted by both the Companies House officer mapper and the \
     planning applicant mapper, through the same slug function, and graph_events records no source";

/// The `location:` namespace's real refusal reason.
pub const LOCATION_NAMESPACE_REASON: &str =
    "location: ids are address-derived and minted by the Companies House, Contracts Finder, \
     planning and FCA Mutuals mappers alike, so one node can be reached from several datasets";

pub fn dated(from: Option<&str>, to: Option<&str>, basis: &str) -> Temporal {
    Temporal::unfiltered(EffectiveRange::Recorded {
        from: from.map(str::to_string),
        to: to.map(str::to_string),
        basis: basis.to_string(),
    })
}

pub fn undated(basis: &str) -> Temporal {
    Temporal::unfiltered(EffectiveRange::Unrecorded {
        basis: basis.to_string(),
    })
}

/// A dated range the as-at filter has positively placed outside the date it was given. The row is
/// still here — nothing is ever removed (45-06) — and the renderer is what marks it.
pub fn out_of_force(from: &str, to: &str, basis: &str) -> Temporal {
    Temporal {
        range: EffectiveRange::Recorded {
            from: Some(from.to_string()),
            to: Some(to.to_string()),
            basis: basis.to_string(),
        },
        status: AsAtStatus::OutOfForce,
    }
}

/// A dated range the as-at filter tested and found covering the date it was given.
pub fn in_force(from: &str, to: Option<&str>, basis: &str) -> Temporal {
    Temporal {
        range: EffectiveRange::Recorded {
            from: Some(from.to_string()),
            to: to.map(str::to_string),
            basis: basis.to_string(),
        },
        status: AsAtStatus::InForce,
    }
}

/// A row the filter **could not test**: no endpoint recorded, so `AsAtStatus::Undated` under the
/// null-range rule — retained and labelled, never dropped and never passed through as though it
/// had been tested (45-06).
///
/// Distinct from [`undated`], which is the same range on a profile with no filter set at all: that
/// row's status is `NotFiltered`, because nothing has asked it a question yet.
pub fn untested(basis: &str) -> Temporal {
    Temporal {
        range: EffectiveRange::Unrecorded {
            basis: basis.to_string(),
        },
        status: AsAtStatus::Undated,
    }
}

pub fn entity_ref(id: &str, name: &str, kind: &str) -> EntityRef {
    EntityRef {
        id: id.to_string(),
        name: name.to_string(),
        kind: kind.to_string(),
    }
}

pub fn page<T>(rows: Vec<T>, total: u64, next: Option<&str>) -> Page<T> {
    Page {
        rows,
        next_cursor: next.map(str::to_string),
        total,
    }
}

pub fn empty_page<T>() -> Page<T> {
    Page {
        rows: Vec::new(),
        next_cursor: None,
        total: 0,
    }
}

/// A profile with at least one row in every section.
///
/// Deliberately mixed: the address row is unattributed (100% of addresses are, corpus-wide — see
/// 45-04), the OSINT link carries a `Confirmed` tier, the contract pivots to a `Contract` node
/// that has no profile route, and the related entity pivots to a company that does.
pub fn sample_profile() -> EntityProfileDto {
    EntityProfileDto {
        id: "GB-COH:X6999001".to_string(),
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
            value: "X6999001".to_string(),
            provenance: attributed("CH"),
        }],
        officers: page(
            vec![OfficerRowView {
                name: "Jane Smith".to_string(),
                role: Some("director".to_string()),
                appointed: Some("2015-01-04".to_string()),
                resigned: Some("2021-09-30".to_string()),
                pivot: None,
                provenance: unattributed(DIRECTOR_NAMESPACE_REASON),
                temporal: dated(
                    Some("2015-01-04"),
                    Some("2021-09-30"),
                    "officer.appointed_on / officer.resigned_on",
                ),
            }],
            2_400,
            Some("officers@20"),
        ),
        addresses: page(
            vec![AddressRowView {
                postcode: Some("XX1 1XX".to_string()),
                address_line_1: Some("1 Example Street".to_string()),
                locality: None,
                pivot: Some(entity_ref(
                    "location:1-example-street",
                    "1 Example Street",
                    "Location",
                )),
                provenance: unattributed(LOCATION_NAMESPACE_REASON),
                temporal: undated("located_at.meta.effective_from / located_at.meta.effective_to"),
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
                pivot: Some(entity_ref(
                    "CF:x9995-cf-00001",
                    "Highway resurfacing",
                    "Contract",
                )),
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
                pivot: Some(entity_ref("planning:48C93", "48C93", "Contract")),
                provenance: attributed("LPA"),
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
                pivot: Some(entity_ref(
                    "PA:example-party",
                    "Example Party",
                    "PoliticalActor",
                )),
                provenance: linked("EC", LinkTier::Confirmed),
            }],
            1,
            None,
        ),
        beneficial_owners: page(
            vec![BeneficialOwnerRowView {
                name: "Jane Smith".to_string(),
                natures_of_control: vec!["ownership-of-shares-75-to-100-percent".to_string()],
                pivot: None,
                provenance: linked("CH", LinkTier::AutoLinked),
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
                temporal: undated("trustee_of.meta.effective_from / trustee_of.meta.effective_to"),
            }],
            1,
            None,
        ),
        donates_to: page(
            vec![LinkedRecordRowView {
                label: "Example Party".to_string(),
                pivot: None,
                provenance: linked("EC", LinkTier::Confirmed),
                temporal: undated("donates_to.meta.effective_from / donates_to.meta.effective_to"),
            }],
            1,
            None,
        ),
        offshore_links: page(
            vec![LinkedRecordRowView {
                label: "Example Offshore Holdings".to_string(),
                pivot: None,
                provenance: unattributed("no source event found for this row"),
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
                ch_number: Some("X6999002".to_string()),
                status: EntityStatusView::Active,
                shared_connection: SharedConnectionView {
                    kind: "director".to_string(),
                    value: "Jane Smith".to_string(),
                },
                pivot: Some(entity_ref(
                    "GB-COH:X6999002",
                    "Example Trading Limited",
                    "Entity",
                )),
                provenance: unattributed(DIRECTOR_NAMESPACE_REASON),
            }],
            48,
            Some("related@20"),
        ),
        lpa_presence: vec![LpaPresenceView {
            lpa_code: "X06999001".to_string(),
            lpa_name: None,
            contract_count: 12,
            planning_application_count: 3,
        }],
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
        as_at: None,
    }
}

/// A profile whose every section is empty, with the findings outcome that means "no probe capable
/// of naming an entity contributed a finding at all".
///
/// The two probe counts are 44-21's measured figures — twelve of thirteen probes interpolate
/// counts, producer names or adapter names and never an id — because the findings empty state
/// renders them and a fixture that invented them would prove nothing about the sentence an analyst
/// actually reads.
pub fn empty_profile() -> EntityProfileDto {
    EntityProfileDto {
        identifiers: Vec::new(),
        officers: empty_page(),
        addresses: empty_page(),
        contracts: empty_page(),
        planning_applications: empty_page(),
        property: empty_page(),
        osint_links: empty_page(),
        beneficial_owners: empty_page(),
        disqualifications: empty_page(),
        trustee_of: empty_page(),
        donates_to: empty_page(),
        offshore_links: empty_page(),
        sanctions: None,
        related: empty_page(),
        lpa_presence: Vec::new(),
        findings: FindingsSectionView {
            findings: empty_page(),
            alerts: empty_page(),
            osint_alerts: empty_page(),
            outcome: FindingsOutcomeView::NotSearched,
            probes_total: 13,
            probes_unable_to_name_an_entity: 12,
        },
        ..sample_profile()
    }
}
