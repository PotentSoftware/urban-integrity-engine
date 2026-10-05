use chrono::Utc;
use urban_integrity_core::pipeline::{EventPayload, EventSource, IngestEvent};
use urban_integrity_schema::types::{
    edge::{DirectsData, EdgeMeta, EdgeWeight, LocatedAtData},
    id::EntityId,
    node::{EntityData, EntityStatus, LocationData, NodeWeight, OfficerData},
};

use crate::address::normalise_address;
use crate::ch::types::ChEvent;

/// Maps CH API company_status string + date_of_cessation to a typed EntityStatus.
/// Date string format: "YYYY-MM-DD" (ISO8601 date, no time component).
fn map_entity_status(status: Option<&str>, date_of_cessation: Option<&str>) -> EntityStatus {
    match status {
        Some("dissolved") => {
            if let Some(date_str) = date_of_cessation {
                match chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    Ok(d) => {
                        let dt = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
                        return EntityStatus::Dissolved(dt);
                    }
                    Err(e) => {
                        tracing::warn!(
                            date = date_str,
                            error = %e,
                            "Failed to parse date_of_cessation — defaulting to Active"
                        );
                    }
                }
            }
            // dissolved with no parseable date: treat as dissolved at epoch
            EntityStatus::Dissolved(chrono::DateTime::UNIX_EPOCH)
        }
        Some("liquidation") | Some("receivership") | Some("administration") => {
            if let Some(date_str) = date_of_cessation {
                if let Ok(d) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                    let dt = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
                    return EntityStatus::InLiquidation(dt);
                }
            }
            EntityStatus::InLiquidation(chrono::DateTime::UNIX_EPOCH)
        }
        Some("active") | None => EntityStatus::Active,
        Some(other) => {
            tracing::debug!(
                status = other,
                "Unknown company_status — defaulting to Active"
            );
            EntityStatus::Active
        }
    }
}

/// Map a Companies House streaming event to 0-N IngestEvents.
///
/// Returns an empty Vec for unknown or unsupported resource kinds (silently
/// dropped — not an error).
pub fn map_ch_event(event: &ChEvent) -> Vec<IngestEvent> {
    match event.resource_kind.as_str() {
        "company-profile" => map_company(event),
        "officer-appointment" => map_officer(event),
        _ => vec![],
    }
}

fn map_company(event: &ChEvent) -> Vec<IngestEvent> {
    let mut events = vec![];
    let d = &event.data;

    let company_id = EntityId(format!("GB-COH:{}", event.resource_id));
    let weight = NodeWeight::Entity(EntityData {
        name: d.company_name.clone().unwrap_or_default(),
        company_number: Some(event.resource_id.clone()),
        status: map_entity_status(d.company_status.as_deref(), d.date_of_cessation.as_deref()),
        incorporation_date: d
            .date_of_creation
            .as_deref()
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()),
    });

    events.push(IngestEvent {
        source: EventSource::CompaniesHouse,
        payload: EventPayload::NodeUpsert {
            id: company_id.clone(),
            weight,
        },
    });

    // Location node from registered_office_address
    if let Some(addr) = &d.registered_office_address {
        if let Some(loc_id) =
            normalise_address(addr.postal_code.as_deref(), addr.address_line_1.as_deref())
        {
            let loc_weight = NodeWeight::Location(LocationData {
                name: addr.address_line_1.clone().unwrap_or_default(),
                postcode: addr.postal_code.clone(),
                address_line_1: addr.address_line_1.clone(),
                locality: addr.locality.clone(),
            });
            events.push(IngestEvent {
                source: EventSource::CompaniesHouse,
                payload: EventPayload::NodeUpsert {
                    id: loc_id.clone(),
                    weight: loc_weight,
                },
            });
            // LocatedAt edge: Company → Location
            let edge = EdgeWeight::LocatedAt {
                meta: EdgeMeta {
                    created_at: Utc::now(),
                    effective_from: None,
                    effective_to: None,
                },
                data: LocatedAtData {
                    details: "registered_office".to_string(),
                },
            };
            events.push(IngestEvent {
                source: EventSource::CompaniesHouse,
                payload: EventPayload::EdgeUpsert {
                    src: company_id,
                    dst: loc_id,
                    weight: edge,
                },
            });
        }
    }
    events
}

fn map_officer(event: &ChEvent) -> Vec<IngestEvent> {
    let d = &event.data;
    let company_id = EntityId(format!("GB-COH:{}", event.resource_id));

    // Officer key: `director:{slug}`, NOT company-scoped. The load-bearing keying rule —
    // one human is one node, across every appointment — is what makes the Phoenix motif
    // (seed entity -> officer -> dissolved entity) traversable at all. This mapper emitted
    // `OFF:{company}-{NAME}` until 2026-08-14; the data written under that form was re-keyed
    // by scripts/rekey_officers_to_director_slug.py (Open Item 17), but the producer was not
    // fixed at the time because nothing yet subscribed to the officer-appointment stream.
    //
    // Use the SHIPPED slug function. Two near-identical ones exist — `naming::slugify` joins
    // on '_', `applicant_slug` on '-' — and the planning mapper keys directors with the
    // latter, so anything else here creates a keyspace that never joins.
    let name = match d.name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n,
        _ => {
            tracing::warn!(
                company = %event.resource_id,
                "CH officer-appointment carries no name — skipping; keying it would fuse \
                 every unnamed officer into one node"
            );
            return Vec::new();
        }
    };
    let slug = crate::planning::mapper::applicant_slug(name);
    if slug.is_empty() {
        tracing::warn!(
            company = %event.resource_id,
            name = %name,
            "CH officer name slugifies to empty — skipping rather than emitting a bare key"
        );
        return Vec::new();
    }
    let officer_id = EntityId(format!("director:{slug}"));

    let officer_weight = NodeWeight::Officer(OfficerData {
        name: name.to_string(),
        officer_role: d.officer_role.clone(),
        appointed_on: d.appointed_on.clone(),
        resigned_on: d.resigned_on.clone(),
        // The company scope the key no longer carries. `build_officer_id_set` in
        // `crates/enricher/src/adapters/ch_psc.rs` is the intended reader — it exists to keep
        // the PSC join company-scoped without reverting the `director:{slug}` key.
        //
        // It does NOT read it yet. That function still recovers the company by parsing it back
        // out of the entity id (`officer_company_number`), which a `director:{slug}` key has
        // nothing to give, so it currently indexes every officer under a bare name key. Wiring
        // it to this field is the consumer half of Open Item 17 and is not in this change.
        company_number: Some(event.resource_id.clone()),
    });

    let edge = EdgeWeight::Directs {
        meta: EdgeMeta {
            created_at: Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DirectsData {
            details: d.officer_role.clone().unwrap_or_default(),
            officer_role: d.officer_role.clone(),
        },
    };

    vec![
        IngestEvent {
            source: EventSource::CompaniesHouse,
            payload: EventPayload::NodeUpsert {
                id: officer_id.clone(),
                weight: officer_weight,
            },
        },
        IngestEvent {
            source: EventSource::CompaniesHouse,
            payload: EventPayload::EdgeUpsert {
                src: officer_id,
                dst: company_id,
                weight: edge,
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn company_event_json() -> &'static str {
        r#"{
            "event": {
                "timepoint": 12345,
                "published_at": "2024-01-01T00:00:00Z",
                "type": "changed",
                "fields_changed": []
            },
            "resource_id": "12345678",
            "resource_kind": "company-profile",
            "data": {
                "company_name": "ACME LTD",
                "company_number": "12345678",
                "company_status": "active",
                "registered_office_address": {
                    "address_line_1": "1 High Street",
                    "locality": "London",
                    "postal_code": "EC1A1BB"
                }
            }
        }"#
    }

    fn officer_event_json() -> &'static str {
        r#"{
            "event": {
                "timepoint": 12346,
                "published_at": "2024-01-01T00:00:00Z",
                "type": "changed",
                "fields_changed": []
            },
            "resource_id": "12345678",
            "resource_kind": "officer-appointment",
            "data": {
                "name": "Jane Smith",
                "officer_role": "director",
                "appointed_on": "2020-01-01"
            }
        }"#
    }

    #[test]
    fn test_map_company_event_produces_entity_node() {
        let event: ChEvent = serde_json::from_str(company_event_json()).unwrap();
        let events = map_ch_event(&event);
        assert!(
            !events.is_empty(),
            "Should produce at least one IngestEvent"
        );
        // First event must be entity NodeUpsert
        match &events[0].payload {
            EventPayload::NodeUpsert { id, weight } => {
                assert!(
                    id.0.contains("GB-COH:12345678"),
                    "EntityId must include company number"
                );
                if let NodeWeight::Entity(e) = weight {
                    assert_eq!(e.name, "ACME LTD");
                    assert_eq!(e.company_number.as_deref(), Some("12345678"));
                    // active company_status maps to EntityStatus::Active
                    assert!(matches!(e.status, EntityStatus::Active));
                } else {
                    panic!("Expected Entity NodeWeight");
                }
            }
            _ => panic!("Expected NodeUpsert"),
        }
    }

    #[test]
    fn test_map_company_event_produces_location_and_edge() {
        let event: ChEvent = serde_json::from_str(company_event_json()).unwrap();
        let events = map_ch_event(&event);
        // Company node + Location node + LocatedAt edge = 3
        assert_eq!(
            events.len(),
            3,
            "Company with valid address should produce 3 events"
        );
    }

    #[test]
    fn test_map_officer_event_produces_officer_and_directs_edge() {
        let event: ChEvent = serde_json::from_str(officer_event_json()).unwrap();
        let events = map_ch_event(&event);
        assert_eq!(
            events.len(),
            2,
            "Officer event should produce officer node + Directs edge"
        );
    }

    /// Pull the `NodeUpsert` id out of a mapped event list.
    fn upserted_node_id(events: &[IngestEvent]) -> String {
        events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::NodeUpsert { id, .. } => Some(id.0.clone()),
                _ => None,
            })
            .expect("expected a NodeUpsert event")
    }

    /// Build an officer-appointment event for `name` at `company`.
    fn officer_event_for(company: &str, name: &str) -> ChEvent {
        let json = format!(
            r#"{{
                "event": {{
                    "timepoint": 1,
                    "published_at": "2024-01-01T00:00:00Z",
                    "type": "changed",
                    "fields_changed": []
                }},
                "resource_id": "{company}",
                "resource_kind": "officer-appointment",
                "data": {{ "name": "{name}", "officer_role": "director" }}
            }}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn officer_nodes_are_keyed_director_slug_not_per_company() {
        // The load-bearing rule: directors are keyed `director:{slug}` so one human is
        // one node. The CH officer-appointment stream used to emit `OFF:{company}-{NAME}`, which
        // gave a serial director one node per appointment and made the Phoenix motif
        // (seed entity -> officer -> dissolved entity) untraversable.
        let events = map_ch_event(&officer_event_for("12345678", "Jane Smith"));
        assert_eq!(upserted_node_id(&events), "director:jane-smith");
    }

    #[test]
    fn the_same_director_at_two_companies_is_one_node() {
        // This is the invariant the key form exists to protect. Without it Phoenix cannot
        // traverse, because no officer node is ever shared by two companies.
        let a = map_ch_event(&officer_event_for("00000001", "Jane Smith"));
        let b = map_ch_event(&officer_event_for("00000002", "Jane Smith"));
        assert_eq!(
            upserted_node_id(&a),
            upserted_node_id(&b),
            "one human must be one node across appointments"
        );
    }

    #[test]
    fn officer_slug_matches_the_shipped_applicant_slug_exactly() {
        // Two near-identical slug functions exist in this codebase: `naming::slugify` joins on
        // '_', `applicant_slug` on '-'. Picking the wrong one silently creates a third disjoint
        // keyspace, which is the bug this test exists to prevent recurring. Call the shipped
        // function; never re-implement it here.
        for name in [
            "Jane Smith",
            "MacDonald-Smythe, Alistair J.",
            "O'Brien   Patrick",
            "Ann-Marie   de la Rue",
        ] {
            let events = map_ch_event(&officer_event_for("12345678", name));
            let expected = format!("director:{}", crate::planning::mapper::applicant_slug(name));
            assert_eq!(
                upserted_node_id(&events),
                expected,
                "officer key for {name:?} must use the shipped applicant_slug"
            );
        }
    }

    #[test]
    fn a_nameless_officer_does_not_collapse_into_a_shared_node() {
        // `unwrap_or("unknown")` would key every unnamed officer as `director:unknown`, fusing
        // unrelated people into one node — the same class of false merge the officer-merge audit
        // found in 14.5% of name-only keys. An officer with no name must produce no node.
        let json = r#"{
            "event":{"timepoint":1,"published_at":"2024-01-01T00:00:00Z","type":"changed","fields_changed":[]},
            "resource_id":"12345678",
            "resource_kind":"officer-appointment",
            "data":{"officer_role":"director"}
        }"#;
        let event: ChEvent = serde_json::from_str(json).unwrap();
        assert!(
            map_ch_event(&event).is_empty(),
            "an officer with no name must not be keyed"
        );
    }

    #[test]
    fn test_unknown_resource_kind_returns_empty() {
        let json = r#"{
            "event":{"timepoint":1,"published_at":"2024-01-01T00:00:00Z","type":"changed","fields_changed":[]},
            "resource_id":"12345",
            "resource_kind":"unknown-kind",
            "data":{}
        }"#;
        let event: ChEvent = serde_json::from_str(json).unwrap();
        assert!(map_ch_event(&event).is_empty());
    }

    #[test]
    fn test_map_entity_status_dissolved_with_date() {
        let status = map_entity_status(Some("dissolved"), Some("2023-04-15"));
        assert!(matches!(status, EntityStatus::Dissolved(_)));
    }

    #[test]
    fn test_map_entity_status_active() {
        let status = map_entity_status(Some("active"), None);
        assert!(matches!(status, EntityStatus::Active));
    }

    #[test]
    fn test_map_entity_status_none_defaults_active() {
        let status = map_entity_status(None, None);
        assert!(matches!(status, EntityStatus::Active));
    }

    #[test]
    fn test_ch_mapper_captures_date_of_creation() {
        // Feed a ChEvent with data.date_of_creation = Some("2024-03-15")
        let json = r#"{
            "event": {
                "timepoint": 12347,
                "published_at": "2024-01-01T00:00:00Z",
                "type": "changed",
                "fields_changed": []
            },
            "resource_id": "09876543",
            "resource_kind": "company-profile",
            "data": {
                "company_name": "NEWCO LTD",
                "company_number": "09876543",
                "company_status": "active",
                "date_of_creation": "2024-03-15"
            }
        }"#;
        let event: ChEvent = serde_json::from_str(json).unwrap();
        let events = map_ch_event(&event);
        assert!(
            !events.is_empty(),
            "Should produce at least one IngestEvent"
        );
        match &events[0].payload {
            EventPayload::NodeUpsert { weight, .. } => {
                if let NodeWeight::Entity(e) = weight {
                    let expected = chrono::NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
                    assert_eq!(
                        e.incorporation_date,
                        Some(expected),
                        "incorporation_date must be parsed from date_of_creation"
                    );
                } else {
                    panic!("Expected Entity NodeWeight");
                }
            }
            _ => panic!("Expected NodeUpsert"),
        }
    }
}
