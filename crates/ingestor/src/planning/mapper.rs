//! Maps `PlanningPortalItem` records from the Scrapling sidecar into
//! `IngestEvent`s for the core pipeline.
//!
//! Each planning application produces up to 7 events:
//!   - 1 Contract node  (planning:{reference})
//!   - 1 Entity node    (director:{name-slug}) — deduplicates across all applications & LPAs
//!   - 1 Location node  (location:{normalised_address})
//!   - 1 Ward node      (ward:{ward_code})
//!   - 1 AwardedTo edge (Entity → Contract)
//!   - 1 LocatedAt edge (Contract → Location) — D-04: sourced from the specific application,
//!     not the applicant, so the edge is attributable to one site rather than every
//!     application the applicant has ever filed.
//!   - 1 Represents edge (Ward → Contract)
//!
//! Entity key design note:
//!   director:{name-slug} — keyed by applicant name only, NOT application reference.
//!   This enables cross-application and cross-LPA Phoenix motif detection.
//!   See plan 09-02 Task 3 for the full rationale.

use chrono::{NaiveDate, Utc};
use regex::Regex;

use urban_integrity_core::pipeline::{EventPayload, EventSource, IngestEvent};
use urban_integrity_schema::types::{
    edge::{AwardedToData, EdgeMeta, EdgeWeight, LocatedAtData, RepresentsData},
    id::EntityId,
    node::{
        ApplicantProvenance, ContractData, EntityData, EntityStatus, LocationData, NodeWeight,
        WardData,
    },
};

use crate::planning::sidecar::PlanningPortalItem;

/// Convert a scraped `PlanningPortalItem` into `IngestEvent`s ready for the pipeline.
///
/// # Arguments
/// * `item` - scraped planning application data from the sidecar
/// * `ward_code` - the ONS GSS ward code (e.g. "W05001492") for this portal entry
pub fn map_planning_item(item: &PlanningPortalItem, ward_code: &str) -> Vec<IngestEvent> {
    let reference = match &item.reference {
        Some(r) if !r.is_empty() => r.clone(),
        _ => return vec![], // Cannot create nodes without a reference — skip
    };

    let description = item.description.clone().unwrap_or_default();
    let address = item.address.clone().unwrap_or_default();
    let applicant = item
        .applicant
        .clone()
        .unwrap_or_else(|| "Unknown Applicant".to_string());
    let ward_name = item.ward.clone().unwrap_or_else(|| ward_code.to_string());

    let contract_id = EntityId(format!("planning:{reference}"));

    // IMPORTANT: Entity is keyed by applicant NAME ONLY (not by application reference).
    // This is the critical schema decision that enables cross-application and cross-LPA
    // director/applicant deduplication:
    //
    //   ✅ director:{slug}                           → same node across all applications
    //   ❌ planning-applicant:{reference}:{slug}     → siloed per application — blocks Phoenix detection
    //
    // If "Apex Build Ltd" appears in 10 planning applications across Anglesey and Gwynedd,
    // they resolve to ONE graph node, enabling motif detection across the full dataset.
    //
    // When Companies House data is ingested, the CH company-number will be used to
    // further deduplicate and merge these provisional entity nodes.
    let applicant_slug = applicant_slug(&applicant);
    // Key: director:{name-slug} — NOT application-scoped
    let entity_id = EntityId(format!("director:{applicant_slug}"));
    let location_id = location_id_for(&address);
    let ward_id = EntityId(format!("ward:{ward_code}"));
    let provenance = derive_applicant_provenance(item.applicant_provenance.as_deref());

    vec![
        // Contract node
        IngestEvent {
            source: EventSource::PlanningPortal,
            payload: EventPayload::NodeUpsert {
                id: contract_id.clone(),
                weight: NodeWeight::Contract(ContractData {
                    name: description.clone(),
                    ocid: None,
                    value_gbp: None,
                    status: Some("pending".to_string()),
                    date_published: None,
                    ward_code: Some(ward_code.to_string()),
                    tender_ref: None,
                    application_ref: Some(reference.clone()),
                    // D-07: parse ISO date string from sidecar; degrade to None on absent/malformed.
                    application_received_date: item
                        .received_date
                        .as_deref()
                        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()),
                    applicant_provenance: provenance,
                    portal_detail_handle: item.detail_handle.clone(),
                }),
            },
        },
        // Provisional applicant Entity node (source: Planning, no CH lookup)
        IngestEvent {
            source: EventSource::PlanningPortal,
            payload: EventPayload::NodeUpsert {
                id: entity_id.clone(),
                weight: NodeWeight::Entity(EntityData {
                    name: applicant.clone(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            },
        },
        // Location node
        IngestEvent {
            source: EventSource::PlanningPortal,
            payload: EventPayload::NodeUpsert {
                id: location_id.clone(),
                weight: NodeWeight::Location(LocationData {
                    name: address.clone(),
                    postcode: extract_postcode(&address),
                    address_line_1: None,
                    locality: None,
                }),
            },
        },
        // Ward node (upsert — may already exist from other sources)
        IngestEvent {
            source: EventSource::PlanningPortal,
            payload: EventPayload::NodeUpsert {
                id: ward_id.clone(),
                weight: NodeWeight::Ward(WardData { name: ward_name }),
            },
        },
        // AwardedTo edge: Applicant → Contract
        IngestEvent {
            source: EventSource::PlanningPortal,
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
                        details: format!("planning application {reference}"),
                    },
                },
            },
        },
        // LocatedAt edge: Contract (application) → Location (D-04: sourced from the specific
        // application, not the applicant, so the edge is attributable to one site, not every
        // application the applicant has ever filed).
        IngestEvent {
            source: EventSource::PlanningPortal,
            payload: EventPayload::EdgeUpsert {
                src: contract_id.clone(),
                dst: location_id,
                weight: EdgeWeight::LocatedAt {
                    meta: EdgeMeta {
                        created_at: Utc::now(),
                        effective_from: None,
                        effective_to: None,
                    },
                    data: LocatedAtData {
                        details: address.clone(),
                    },
                },
            },
        },
        // Represents edge: Ward → Contract
        IngestEvent {
            source: EventSource::PlanningPortal,
            payload: EventPayload::EdgeUpsert {
                src: ward_id,
                dst: contract_id,
                weight: EdgeWeight::Represents {
                    meta: EdgeMeta {
                        created_at: Utc::now(),
                        effective_from: None,
                        effective_to: None,
                    },
                    data: RepresentsData {
                        details: format!("ward {ward_code}"),
                    },
                },
            },
        },
    ]
}

/// Slugify an applicant name into the lowercase/alphanumeric/hyphen-joined form used for the
/// `director:{slug}` entity key. Extracted verbatim from `map_planning_item` so 24-05's backfill
/// can reuse it exactly, without duplicating (and risking drift from) the ingest-time logic.
pub fn applicant_slug(applicant: &str) -> String {
    applicant
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Compute the `location:{address}` entity id from a normalised address string. Extracted
/// verbatim from `map_planning_item` so 24-05's backfill can reuse it exactly.
pub(crate) fn location_id_for(address: &str) -> EntityId {
    EntityId(format!(
        "location:{}",
        address.to_lowercase().replace(' ', "-")
    ))
}

/// Map the sidecar's `applicant_provenance` wire string (D-02) to the typed enum.
///
/// Returns `None` — "not yet measured" — for an absent or unrecognised value. All three
/// D-02 states are positive claims that require the source to have actually said something:
/// `AbsentAtSource` in particular asserts *the portal publishes nothing for this record*,
/// which is not a safe thing to infer from silence.
///
/// CR-05: the previous version collapsed `None` into `AbsentAtSource` as a "conservative
/// default". It is not conservative. Several live paths emit no provenance at all
/// (`firecrawl.rs:176`, `sidecar.py::scrape_static_css`, the Idox Firecrawl fallback tiers),
/// so a single re-poll that fell back to Firecrawl rewrote every Contract in the ward from a
/// source-verified `Published` down to `AbsentAtSource` — invisible at write time, surfacing
/// later as a CRITICAL coverage regression. `None` is the honest value; the probe already
/// buckets it as `not_yet_measured`.
pub(crate) fn derive_applicant_provenance(raw: Option<&str>) -> Option<ApplicantProvenance> {
    match raw {
        Some("published") => Some(ApplicantProvenance::Published),
        Some("fetch_failed") => Some(ApplicantProvenance::FetchFailed),
        Some("absent_at_source") => Some(ApplicantProvenance::AbsentAtSource),
        _ => None,
    }
}

/// Extract a normalised UK postcode from an address string.
/// Returns `None` if no postcode pattern is found.
pub(crate) fn extract_postcode(address: &str) -> Option<String> {
    // UK postcode regex: AN NAA, ANN NAA, AAN NAA, AANN NAA, ANA NAA, AANA NAA
    let upper = address.to_uppercase();
    let re = Regex::new(r"\b([A-Z]{1,2}[0-9][0-9A-Z]?\s?[0-9][A-Z]{2})\b").ok()?;
    re.find(&upper).map(|m| m.as_str().replace(' ', ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_item(reference: &str, applicant: &str, address: &str) -> PlanningPortalItem {
        PlanningPortalItem {
            reference: Some(reference.to_string()),
            description: Some("Erection of warehouse".to_string()),
            address: Some(address.to_string()),
            ward: Some("Aethwy".to_string()),
            applicant: Some(applicant.to_string()),
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        }
    }

    #[test]
    fn test_map_planning_item_produces_seven_events() {
        let item = fixture_item(
            "FPL/2024/001",
            "Acme Developments Ltd",
            "1 High St, Llangefni LL77 7AA",
        );
        let events = map_planning_item(&item, "W05001492");
        assert_eq!(events.len(), 7, "Should produce 4 nodes + 3 edges");
    }

    #[test]
    fn test_map_planning_item_contract_has_ward_code() {
        let item = fixture_item("FPL/2024/002", "Builder Ltd", "2 Church St LL65 1AB");
        let events = map_planning_item(&item, "W05001492");
        let contract_event = events.iter().find(|e| {
            matches!(&e.payload, EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:"))
        });
        assert!(contract_event.is_some());
        if let Some(IngestEvent {
            payload:
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Contract(c),
                    ..
                },
            ..
        }) = contract_event
        {
            assert_eq!(c.ward_code.as_deref(), Some("W05001492"));
        }
    }

    #[test]
    fn test_map_planning_item_entity_is_provisional() {
        let item = fixture_item("FPL/2024/003", "Local Builder", "3 Main Rd LL77 8BB");
        let events = map_planning_item(&item, "W05001493");
        let entity_event = events.iter().find(|e| {
            matches!(&e.payload, EventPayload::NodeUpsert { id, .. } if id.0.starts_with("director:"))
        });
        assert!(entity_event.is_some());
        if let Some(IngestEvent {
            payload:
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Entity(e),
                    ..
                },
            ..
        }) = entity_event
        {
            assert!(
                e.company_number.is_none(),
                "Provisional entity must have no CH number"
            );
            assert_eq!(e.status, EntityStatus::Active);
        }
    }

    #[test]
    fn test_map_planning_item_entity_key_is_not_application_scoped() {
        // REGRESSION GUARD: entity key must be director:{slug}, not planning-applicant:{ref}:{slug}
        // If this test fails, cross-LPA Phoenix detection is broken.
        let item = fixture_item("FPL/2024/099", "Apex Build Ltd", "10 Bridge St LL77 1AA");
        let events = map_planning_item(&item, "W05001492");
        let entity_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Entity(_),
                    ..
                }
            )
        });
        assert!(entity_event.is_some());
        if let Some(IngestEvent {
            payload: EventPayload::NodeUpsert { id, .. },
            ..
        }) = entity_event
        {
            assert!(
                id.0.starts_with("director:"),
                "Entity key must be director:{{slug}}, got: {}",
                id.0
            );
            assert!(
                !id.0.contains("FPL/2024/099"),
                "Entity key must NOT contain application reference — cross-LPA deduplication broken"
            );
        }
    }

    /// D-04 regression guard: the `LocatedAt` edge must be sourced from the Contract
    /// (the specific application), not the applicant Entity — otherwise every location
    /// a planning application's `LocatedAt` edge points at is attributable to every
    /// application the applicant has ever filed, not the one it actually belongs to.
    #[test]
    fn test_map_planning_item_locatedat_edge_sourced_from_contract_not_applicant() {
        let item = fixture_item(
            "FPL/2024/100",
            "Apex Build Ltd",
            "100 High St, Llangefni LL77 7AA",
        );
        let events = map_planning_item(&item, "W05001492");
        let located_at_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::EdgeUpsert {
                    weight: EdgeWeight::LocatedAt { .. },
                    ..
                }
            )
        });
        assert!(located_at_event.is_some(), "LocatedAt event not found");
        if let Some(IngestEvent {
            payload: EventPayload::EdgeUpsert { src, .. },
            ..
        }) = located_at_event
        {
            assert!(
                src.0.starts_with("planning:"),
                "LocatedAt.src must be sourced from the contract (planning:{{reference}}), got: {}",
                src.0
            );
            assert!(
                !src.0.starts_with("director:"),
                "LocatedAt.src must NOT be sourced from the applicant (director:{{slug}}), got: {}",
                src.0
            );
        }
    }

    #[test]
    fn test_map_planning_item_missing_reference_returns_empty() {
        let item = PlanningPortalItem {
            reference: None,
            description: Some("desc".to_string()),
            address: Some("addr".to_string()),
            ward: None,
            applicant: None,
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001492");
        assert!(
            events.is_empty(),
            "Missing reference must produce no events"
        );
    }

    #[test]
    fn test_extract_postcode_finds_valid_postcode() {
        assert_eq!(
            extract_postcode("1 High Street, Llangefni LL77 7AA"),
            Some("LL777AA".to_string())
        );
    }

    #[test]
    fn test_extract_postcode_returns_none_when_absent() {
        assert_eq!(extract_postcode("Some Rural Address, Anglesey"), None);
    }

    /// REP-04: map_planning_item populates application_ref from the planning reference,
    /// and tender_ref is None (planning data has no CF tender ID).
    #[test]
    fn test_map_planning_item_populates_application_ref() {
        let item = fixture_item(
            "FUL/2024/001234",
            "Apex Build Ltd",
            "1 High St, Llangefni LL77 7AA",
        );
        let events = map_planning_item(&item, "W05001492");
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.application_ref,
                Some("FUL/2024/001234".to_string()),
                "application_ref must be populated from planning reference"
            );
            assert_eq!(
                cd.tender_ref, None,
                "planning contracts must have tender_ref = None"
            );
        }
    }

    /// D-07: received_date Some("YYYY-MM-DD") maps to application_received_date Some(NaiveDate).
    #[test]
    fn test_map_planning_item_received_date_maps_to_naive_date() {
        use chrono::NaiveDate;

        let item = PlanningPortalItem {
            reference: Some("FPL/2024/007".to_string()),
            description: Some("Erection of warehouse".to_string()),
            address: Some("7 Church St LL77 7AA".to_string()),
            ward: Some("Aethwy".to_string()),
            applicant: Some("Builder Ltd".to_string()),
            received_date: Some("2024-05-21".to_string()),
            applicant_provenance: None,
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001492");
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.application_received_date,
                Some(NaiveDate::from_ymd_opt(2024, 5, 21).unwrap()),
                "received_date '2024-05-21' must map to NaiveDate(2024-05-21)"
            );
        }
    }

    /// D-07: received_date None maps to application_received_date None (backward compat).
    #[test]
    fn test_map_planning_item_none_received_date_maps_to_none() {
        let item = fixture_item("FPL/2024/008", "Builder Ltd", "8 Church St LL77 7AA");
        // fixture_item sets received_date: None
        let events = map_planning_item(&item, "W05001492");
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.application_received_date, None,
                "None received_date must map to None application_received_date"
            );
        }
    }

    /// D-07: a malformed received_date string maps to None without panicking.
    #[test]
    fn test_map_planning_item_malformed_received_date_maps_to_none() {
        let item = PlanningPortalItem {
            reference: Some("FPL/2024/009".to_string()),
            description: Some("Erection of warehouse".to_string()),
            address: Some("9 Church St LL77 7AA".to_string()),
            ward: Some("Aethwy".to_string()),
            applicant: Some("Builder Ltd".to_string()),
            received_date: Some("not-a-date".to_string()),
            applicant_provenance: None,
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001492");
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.application_received_date, None,
                "Malformed date must degrade to None, not panic"
            );
        }
    }

    // ── Tests for extracted pub(crate) helpers (Phase 24, D-02/D-04) ────────

    #[test]
    fn test_applicant_slug_matches_current_inline_logic() {
        assert_eq!(applicant_slug("Apex Build Ltd"), "apex-build-ltd");
        assert_eq!(applicant_slug("O'Brien & Sons"), "o-brien-sons");
        assert_eq!(applicant_slug("  Leading Space Ltd"), "leading-space-ltd");
    }

    #[test]
    fn test_location_id_for_matches_current_inline_logic() {
        assert_eq!(
            location_id_for("1 High St, Llangefni LL77 7AA").0,
            "location:1-high-st,-llangefni-ll77-7aa"
        );
    }

    #[test]
    fn test_derive_applicant_provenance_published() {
        assert_eq!(
            derive_applicant_provenance(Some("published")),
            Some(ApplicantProvenance::Published)
        );
    }

    #[test]
    fn test_derive_applicant_provenance_fetch_failed() {
        assert_eq!(
            derive_applicant_provenance(Some("fetch_failed")),
            Some(ApplicantProvenance::FetchFailed)
        );
    }

    /// CR-05: an absent provenance string means "nobody measured this record", NOT
    /// "the portal published nothing". `AbsentAtSource` is a positive claim about the
    /// source and must never be manufactured from silence — several live paths
    /// (`firecrawl.rs`, `scrape_static_css`, the Idox Firecrawl fallback tiers) emit no
    /// provenance at all, and mapping those to `AbsentAtSource` let a single re-poll
    /// silently downgrade a whole ward's measured `Published` records.
    #[test]
    fn test_derive_applicant_provenance_none_is_unmeasured() {
        assert_eq!(derive_applicant_provenance(None), None);
    }

    #[test]
    fn test_derive_applicant_provenance_unrecognised_value_is_unmeasured() {
        assert_eq!(derive_applicant_provenance(Some("something-else")), None);
    }

    #[test]
    fn test_derive_applicant_provenance_absent_at_source_is_explicit_only() {
        assert_eq!(
            derive_applicant_provenance(Some("absent_at_source")),
            Some(ApplicantProvenance::AbsentAtSource)
        );
    }

    #[test]
    fn test_map_planning_item_wires_published_provenance_into_contract() {
        let mut item = fixture_item(
            "FPL/2024/010",
            "Apex Build Ltd",
            "10 High St, Llangefni LL77 7AA",
        );
        item.applicant_provenance = Some("published".to_string());
        let events = map_planning_item(&item, "W05001492");
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.applicant_provenance,
                Some(ApplicantProvenance::Published)
            );
        }
    }

    // ── D-02 regression guard: provenance and the Unknown-Applicant fallback are
    //    independently correct, never conflated (Phase 24, Task 3) ──────────

    #[test]
    fn test_map_planning_item_published_provenance_never_yields_unknown_applicant() {
        let item = PlanningPortalItem {
            reference: Some("FPL/2024/011".to_string()),
            description: Some("Erection of warehouse".to_string()),
            address: Some("11 Church St LL77 7AA".to_string()),
            ward: Some("Aethwy".to_string()),
            applicant: Some("Apex Build Ltd".to_string()),
            received_date: None,
            applicant_provenance: Some("published".to_string()),
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001492");

        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.applicant_provenance,
                Some(ApplicantProvenance::Published)
            );
        }

        let entity_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("director:")
            )
        });
        assert!(entity_event.is_some(), "Entity node event not found");
        if let Some(IngestEvent {
            payload:
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Entity(ed),
                    ..
                },
            ..
        }) = entity_event
        {
            assert_ne!(
                ed.name, "Unknown Applicant",
                "A published applicant must never surface as Unknown Applicant"
            );
        }
    }

    #[test]
    fn test_map_planning_item_fetch_failed_provenance_still_uses_unknown_applicant_fallback() {
        let item = PlanningPortalItem {
            reference: Some("FPL/2024/012".to_string()),
            description: Some("Erection of warehouse".to_string()),
            address: Some("12 Church St LL77 7AA".to_string()),
            ward: Some("Aethwy".to_string()),
            applicant: None,
            received_date: None,
            applicant_provenance: Some("fetch_failed".to_string()),
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001492");

        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("planning:")
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
                cd.applicant_provenance,
                Some(ApplicantProvenance::FetchFailed)
            );
        }

        let entity_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert { id, .. } if id.0.starts_with("director:")
            )
        });
        assert!(entity_event.is_some(), "Entity node event not found");
        if let Some(IngestEvent {
            payload:
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Entity(ed),
                    ..
                },
            ..
        }) = entity_event
        {
            assert_eq!(
                ed.name, "Unknown Applicant",
                "The existing Unknown-Applicant fallback must be preserved when applicant is None"
            );
        }
    }
}
