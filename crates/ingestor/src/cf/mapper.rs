use chrono::Utc;
use urban_integrity_core::pipeline::{EventPayload, EventSource, IngestEvent};
use urban_integrity_schema::types::{
    edge::{AwardedToData, EdgeMeta, EdgeWeight, IssuedByData, LocatedAtData},
    id::EntityId,
    node::{ContractData, EntityData, EntityStatus, LocationData, NodeWeight},
};

use crate::address::normalise_address;
use crate::cf::types::{OcdsParty, OcdsRelease};

/// Map one OCDS release to a Vec of IngestEvents.
///
/// Produces:
/// - 1 Contract node (from release.ocid + tender fields)
/// - N Entity nodes (one per party)
/// - N Location nodes (for parties with valid postcodes)
/// - LocatedAt edges (Entity → Location)
/// - AwardedTo edges (supplier Entity → Contract, from awards[].suppliers)
/// - IssuedBy edges (buyer Entity → Contract)
pub fn map_release(release: &OcdsRelease) -> Vec<IngestEvent> {
    let mut events = vec![];

    let contract_id = EntityId(format!("CF:{}", release.ocid));
    let contract_weight = NodeWeight::Contract(ContractData {
        name: release
            .tender
            .as_ref()
            .and_then(|t| t.title.clone())
            .unwrap_or_else(|| release.ocid.clone()),
        ocid: Some(release.ocid.clone()),
        value_gbp: release
            .tender
            .as_ref()
            .and_then(|t| t.value.as_ref())
            .and_then(|v| v.amount),
        status: release.tender.as_ref().and_then(|t| t.status.clone()),
        // `tender.datePublished` is a Contracts Finder extension and is usually absent —
        // 17 of 100 live releases carry it, against 100 of 100 for `release.date`, which OCDS
        // makes mandatory. Reading only the former left 4,929 of 5,552 CF contracts undated,
        // so Phoenix's |award_date - dissolved_dt| window could not be evaluated at all
        // (planning open item 19). Prefer the more specific field, fall back to the standard
        // one; both are RFC3339 with offset, so the stored shape is unchanged.
        date_published: release
            .tender
            .as_ref()
            .and_then(|t| t.date_published.clone())
            .or_else(|| release.date.clone()),
        ward_code: None, // CF contracts are not ward-specific
        tender_ref: release.tender.as_ref().map(|t| t.id.clone()),
        application_ref: None,
        application_received_date: None, // CF contracts have no planning received date
        applicant_provenance: None,      // CF contracts have no planning applicant provenance
        portal_detail_handle: None,      // CF contracts have no planning portal detail handle
    });

    events.push(IngestEvent {
        source: EventSource::ContractsFinder,
        payload: EventPayload::NodeUpsert {
            id: contract_id.clone(),
            weight: contract_weight,
        },
    });

    // Process parties
    for party in &release.parties {
        let entity_id = party_entity_id(party);
        let entity_weight = NodeWeight::Entity(EntityData {
            name: party.name.clone().unwrap_or_default(),
            company_number: party
                .identifier
                .as_ref()
                .filter(|i| i.scheme.as_deref() == Some("GB-COH"))
                .and_then(|i| i.id.clone()),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        events.push(IngestEvent {
            source: EventSource::ContractsFinder,
            payload: EventPayload::NodeUpsert {
                id: entity_id.clone(),
                weight: entity_weight,
            },
        });

        // Location node from party address
        if let Some(addr) = &party.address {
            if let Some(loc_id) =
                normalise_address(addr.postal_code.as_deref(), addr.street_address.as_deref())
            {
                let loc_weight = NodeWeight::Location(LocationData {
                    name: addr.street_address.clone().unwrap_or_default(),
                    postcode: addr.postal_code.clone(),
                    address_line_1: addr.street_address.clone(),
                    locality: addr.locality.clone(),
                });
                events.push(IngestEvent {
                    source: EventSource::ContractsFinder,
                    payload: EventPayload::NodeUpsert {
                        id: loc_id.clone(),
                        weight: loc_weight,
                    },
                });
                events.push(IngestEvent {
                    source: EventSource::ContractsFinder,
                    payload: EventPayload::EdgeUpsert {
                        src: entity_id.clone(),
                        dst: loc_id,
                        weight: EdgeWeight::LocatedAt {
                            meta: EdgeMeta {
                                created_at: Utc::now(),
                                effective_from: None,
                                effective_to: None,
                            },
                            data: LocatedAtData {
                                details: "party_address".to_string(),
                            },
                        },
                    },
                });
            }
        }

        // AwardedTo edge: supplier → contract (matched via awards[].suppliers)
        if party.roles.contains(&"supplier".to_string()) {
            if let Some(awards) = &release.awards {
                for award in awards {
                    if award.suppliers.iter().any(|s| s.id == party.id) {
                        events.push(IngestEvent {
                            source: EventSource::ContractsFinder,
                            payload: EventPayload::EdgeUpsert {
                                src: entity_id.clone(),
                                dst: contract_id.clone(),
                                weight: EdgeWeight::AwardedTo {
                                    meta: EdgeMeta {
                                        created_at: Utc::now(),
                                        effective_from: None,
                                        effective_to: None,
                                    },
                                    data: AwardedToData {
                                        details: format!("award:{}", award.id),
                                    },
                                },
                            },
                        });
                    }
                }
            }
        }

        // IssuedBy edge: buyer → contract
        if party.roles.contains(&"buyer".to_string()) {
            events.push(IngestEvent {
                source: EventSource::ContractsFinder,
                payload: EventPayload::EdgeUpsert {
                    src: entity_id,
                    dst: contract_id.clone(),
                    weight: EdgeWeight::IssuedBy {
                        meta: EdgeMeta {
                            created_at: Utc::now(),
                            effective_from: None,
                            effective_to: None,
                        },
                        data: IssuedByData {
                            details: "buyer".to_string(),
                        },
                    },
                },
            });
        }
    }

    events
}

/// Derive a stable `EntityId` for a party.
///
/// Prefers `GB-COH:{company_number}` when available (enables cross-reference
/// with the CH stream). Falls back to `CF-PARTY:{cf_id}`.
pub fn party_entity_id(party: &OcdsParty) -> EntityId {
    if let Some(coh_id) = party
        .identifier
        .as_ref()
        .filter(|i| i.scheme.as_deref() == Some("GB-COH"))
        .and_then(|i| i.id.as_ref())
    {
        return EntityId(format!("GB-COH:{}", coh_id));
    }
    EntityId(format!("CF-PARTY:{}", party.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live OCDS fixture from the Contracts Finder API (NHS England / Diabetic Eye Screening)
    const AWARD_FIXTURE: &str = r#"{
        "ocid": "ocds-b5fd17-fe9928c3-0038-44ad-ab3e-7da3d644ae8c",
        "id": "9ee491a9-8ae7-43dd-8b5c-673f73dfcc57-896512",
        "language": "en",
        "date": "2026-05-08T18:25:52+01:00",
        "tag": ["award"],
        "initiationType": "tender",
        "tender": {
            "id": "CF-3526400D0O000000rwimUAA",
            "title": "Diabetic Eye Screening Data Collection",
            "status": "complete",
            "value": { "amount": 255040.0, "currency": "GBP" },
            "datePublished": "2026-05-08T18:25:52+01:00"
        },
        "parties": [
            {
                "id": "GB-CFS-260069",
                "name": "NHS England",
                "identifier": { "legalName": "NHS England" },
                "address": {
                    "streetAddress": "7 Wellington Place",
                    "locality": "Leeds",
                    "postalCode": "LS14AP",
                    "countryName": "United Kingdom"
                },
                "roles": ["buyer"]
            },
            {
                "id": "GB-CFS-334495",
                "name": "Health Information Consulting Limited",
                "identifier": { "legalName": "Health Information Consulting Limited" },
                "address": { "streetAddress": "Low Barn, Faversham" },
                "roles": ["supplier"]
            }
        ],
        "buyer": { "id": "GB-CFS-260069", "name": "NHS England" },
        "awards": [{
            "id": "ocds-b5fd17-fe9928c3-0038-44ad-ab3e-7da3d644ae8c-1",
            "status": "active",
            "date": "2026-05-06T00:00:00+01:00",
            "value": { "amount": 255040.0, "currency": "GBP" },
            "suppliers": [{ "id": "GB-CFS-334495", "name": "Health Information Consulting Limited" }]
        }]
    }"#;

    #[test]
    fn test_map_release_produces_contract_node() {
        let release: OcdsRelease = serde_json::from_str(AWARD_FIXTURE).unwrap();
        let events = map_release(&release);
        let contract_events: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.payload,
                    EventPayload::NodeUpsert { id, .. } if id.0.starts_with("CF:")
                )
            })
            .collect();
        assert_eq!(
            contract_events.len(),
            1,
            "Should produce exactly one Contract node"
        );
    }

    #[test]
    fn test_map_release_produces_supplier_awarded_to_edge() {
        let release: OcdsRelease = serde_json::from_str(AWARD_FIXTURE).unwrap();
        let events = map_release(&release);
        let award_edges: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.payload,
                    EventPayload::EdgeUpsert { weight, .. }
                        if matches!(weight, EdgeWeight::AwardedTo { .. })
                )
            })
            .collect();
        assert!(
            !award_edges.is_empty(),
            "Should produce at least one AwardedTo edge for the supplier"
        );
    }

    #[test]
    fn test_map_release_buyer_gets_issued_by_edge() {
        let release: OcdsRelease = serde_json::from_str(AWARD_FIXTURE).unwrap();
        let events = map_release(&release);
        let issued_edges: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.payload,
                    EventPayload::EdgeUpsert { weight, .. }
                        if matches!(weight, EdgeWeight::IssuedBy { .. })
                )
            })
            .collect();
        assert!(
            !issued_edges.is_empty(),
            "Buyer should get an IssuedBy edge to the contract"
        );
    }

    #[test]
    fn test_party_entity_id_prefers_gb_coh() {
        use crate::cf::types::{OcdsIdentifier, OcdsParty};
        let party = OcdsParty {
            id: "GB-CFS-999".to_string(),
            name: Some("ACME Ltd".to_string()),
            identifier: Some(OcdsIdentifier {
                scheme: Some("GB-COH".to_string()),
                id: Some("12345678".to_string()),
                legal_name: None,
            }),
            address: None,
            roles: vec![],
        };
        let eid = party_entity_id(&party);
        assert_eq!(eid.0, "GB-COH:12345678", "Should prefer GB-COH identifier");
    }

    #[test]
    fn test_party_entity_id_falls_back_to_cf_party() {
        use crate::cf::types::OcdsParty;
        let party = OcdsParty {
            id: "GB-CFS-123".to_string(),
            name: Some("Some Buyer".to_string()),
            identifier: None,
            address: None,
            roles: vec![],
        };
        let eid = party_entity_id(&party);
        assert_eq!(
            eid.0, "CF-PARTY:GB-CFS-123",
            "Should fall back to CF-PARTY: prefix"
        );
    }

    /// REP-04: map_release with a release containing tender.id populates tender_ref on ContractData.
    #[test]
    fn test_map_release_populates_tender_ref() {
        let release: OcdsRelease = serde_json::from_str(AWARD_FIXTURE).unwrap();
        let events = map_release(&release);
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("CF:")
            )
        });
        assert!(contract_event.is_some(), "Contract node event not found");
        if let Some(IngestEvent {
            payload:
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Contract(cd),
                    ..
                },
            ..
        }) = contract_event
        {
            assert_eq!(
                cd.tender_ref,
                Some("CF-3526400D0O000000rwimUAA".to_string()),
                "tender_ref must be populated from release.tender.id"
            );
            assert_eq!(
                cd.application_ref, None,
                "CF contracts must have application_ref = None"
            );
        }
    }

    /// REP-04: map_release with tender: None yields tender_ref = None.
    #[test]
    fn test_map_release_no_tender_yields_none() {
        // Minimal release with no tender field
        const NO_TENDER_FIXTURE: &str = r#"{
            "ocid": "ocds-test-no-tender-001",
            "id": "test-release-001",
            "tag": ["planning"],
            "parties": []
        }"#;
        let release: OcdsRelease = serde_json::from_str(NO_TENDER_FIXTURE).unwrap();
        let events = map_release(&release);
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("CF:")
            )
        });
        assert!(contract_event.is_some(), "Contract node event not found");
        if let Some(IngestEvent {
            payload:
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Contract(cd),
                    ..
                },
            ..
        }) = contract_event
        {
            assert_eq!(
                cd.tender_ref, None,
                "tender_ref must be None when release.tender is None"
            );
        }
    }

    /// `date_published` must fall back to `release.date` when `tender.datePublished` is absent.
    ///
    /// `tender.datePublished` is a Contracts Finder extension and is usually NOT present:
    /// measured live against the CF OCDS API on 2026-08-10, **100 of 100** releases carried
    /// `release.date` (mandatory in OCDS) while only **17** carried `tender.datePublished`.
    /// Reading only the latter left 4,929 of 5,552 CF contracts with no date at all, which
    /// is why Phoenix could not evaluate its `|award_date - dissolved_dt| <= 180` window for
    /// any of its 3 candidate motifs — the window was not rejecting them, it could not run
    /// (planning open item 19).
    ///
    /// Formats are compatible: the 623 rows that already had a value are RFC3339 with offset
    /// (`2026-06-30T17:20:14+01:00`), the same shape `release.date` provides.
    #[test]
    fn date_published_falls_back_to_release_date() {
        const NO_TENDER_DATE: &str = r#"{
            "ocid": "ocds-test-reldate-001",
            "id": "test-release-002",
            "date": "2026-08-10T11:55:26+01:00",
            "tag": ["tender"],
            "parties": [],
            "tender": { "id": "T-1", "title": "Thing" }
        }"#;
        let release: OcdsRelease = serde_json::from_str(NO_TENDER_DATE).unwrap();
        let events = map_release(&release);
        let contract = events.iter().find_map(|e| match &e.payload {
            EventPayload::NodeUpsert {
                id,
                weight: NodeWeight::Contract(cd),
                ..
            } if id.0.starts_with("CF:") => Some(cd),
            _ => None,
        });
        let cd = contract.expect("Contract node event not found");
        assert_eq!(
            cd.date_published.as_deref(),
            Some("2026-08-10T11:55:26+01:00"),
            "with tender.datePublished absent, date_published must come from release.date"
        );
    }

    /// The fallback must not override a present `tender.datePublished` — that field is the
    /// more specific of the two and stays authoritative where CF supplies it.
    #[test]
    fn tender_date_published_wins_over_release_date() {
        const BOTH_DATES: &str = r#"{
            "ocid": "ocds-test-reldate-002",
            "id": "test-release-003",
            "date": "2026-08-10T11:55:26+01:00",
            "tag": ["tender"],
            "parties": [],
            "tender": { "id": "T-2", "title": "Thing", "datePublished": "2026-01-02T09:00:00Z" }
        }"#;
        let release: OcdsRelease = serde_json::from_str(BOTH_DATES).unwrap();
        let events = map_release(&release);
        let contract = events.iter().find_map(|e| match &e.payload {
            EventPayload::NodeUpsert {
                id,
                weight: NodeWeight::Contract(cd),
                ..
            } if id.0.starts_with("CF:") => Some(cd),
            _ => None,
        });
        let cd = contract.expect("Contract node event not found");
        assert_eq!(
            cd.date_published.as_deref(),
            Some("2026-01-02T09:00:00Z"),
            "tender.datePublished must take precedence over release.date"
        );
    }
}
