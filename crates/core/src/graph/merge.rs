//! Node-weight merge for upsert semantics.
//!
//! The ingestion pipeline emits [`GraphMutation::UpsertNode`](crate::graph::types::GraphMutation),
//! which — unlike the insert-only `AddNode` path — must fold a re-ingested weight
//! into the weight already stored for that node. This module holds the pure,
//! side-effect-free merge used by both the live coordinator and the SurrealDB
//! replay path so the two can never diverge.
//!
//! Design decisions (summarised in docs/ARCHITECTURE.md, "Status transitions"):
//!
//! - **Never lose enrichment.** For optional fields the rule is
//!   `incoming.or(existing)`: a fresh value wins, but an absent (`None`) incoming
//!   value never nulls out a value another source already populated. This keeps a
//!   provisional planning re-scrape (which has no `company_number`) from wiping the
//!   Companies House number stamped by `backfill`.
//! - **Monotonic status (precedence guard).** `Dissolved` / `InLiquidation` are
//!   terminal: a later `Active` never downgrades them. This protects against
//!   out-of-order or replayed events. (A stricter timepoint-ordered rule can be
//!   layered on later without changing this signature.)
//! - **Only `Entity` and `Contract` carry a real field-merge**, because those are
//!   the node types the ingestion pipeline updates in place (company status,
//!   contract award status/value). All other variants take the incoming weight
//!   (last-writer-wins); the OSINT variants never arrive via `UpsertNode`.

use urban_integrity_schema::types::node::{
    ApplicantProvenance, ContractData, EntityData, EntityStatus, NodeWeight,
};

/// Merge `incoming` into `existing`, returning the weight that should be stored.
///
/// Pure and deterministic — no I/O, no logging. Callers decide whether the result
/// differs from `existing` (and therefore whether to emit a `NodeUpdated` event).
pub fn merge_node_weight(existing: &NodeWeight, incoming: NodeWeight) -> NodeWeight {
    match (existing, incoming) {
        (NodeWeight::Entity(e), NodeWeight::Entity(i)) => NodeWeight::Entity(EntityData {
            name: pick_name(&e.name, i.name),
            company_number: i.company_number.or_else(|| e.company_number.clone()),
            status: merge_status(&e.status, i.status),
            incorporation_date: i.incorporation_date.or(e.incorporation_date),
        }),
        (NodeWeight::Contract(e), NodeWeight::Contract(i)) => NodeWeight::Contract(ContractData {
            name: pick_name(&e.name, i.name),
            ocid: i.ocid.or_else(|| e.ocid.clone()),
            value_gbp: i.value_gbp.or(e.value_gbp),
            status: i.status.or_else(|| e.status.clone()),
            date_published: i.date_published.or_else(|| e.date_published.clone()),
            ward_code: i.ward_code.or_else(|| e.ward_code.clone()),
            tender_ref: i.tender_ref.or_else(|| e.tender_ref.clone()),
            application_ref: i.application_ref.or_else(|| e.application_ref.clone()),
            application_received_date: i.application_received_date.or(e.application_received_date),
            applicant_provenance: merge_applicant_provenance(
                &e.applicant_provenance,
                i.applicant_provenance,
            ),
            portal_detail_handle: i
                .portal_detail_handle
                .or_else(|| e.portal_detail_handle.clone()),
        }),
        // Officer / Location / Ward / PoliticalActor and the OSINT variants:
        // last-writer-wins. (A blank incoming name is preserved as-is; the ingestion
        // mappers never emit blank names for these.) A variant mismatch for the same
        // id should not occur; taking the incoming value is the safe default.
        (_, incoming) => incoming,
    }
}

/// Provenance merge (precedence guard) — a measured `Published` is never downgraded.
///
/// CR-05: this used to be plain incoming-wins. Combined with a mapper that manufactured
/// `AbsentAtSource` out of an absent provenance string, a single re-poll of a ward that fell
/// back to Firecrawl (or any generic-CSS portal, neither of which emits provenance) rewrote
/// every Contract in that ward from a source-verified `Published` down to `AbsentAtSource` —
/// silently, and surfacing much later as a CRITICAL coverage regression.
///
/// `Published` is the strongest claim available: it means the portal named the applicant and
/// we recorded it. Nothing weaker should be able to overwrite it, and `None` ("not measured")
/// should never overwrite anything at all.
fn merge_applicant_provenance(
    existing: &Option<ApplicantProvenance>,
    incoming: Option<ApplicantProvenance>,
) -> Option<ApplicantProvenance> {
    match (existing, &incoming) {
        // A stored measurement of `Published` outranks any later, weaker claim.
        (Some(ApplicantProvenance::Published), _) => existing.clone(),
        // Otherwise an affirmative incoming claim wins (including a fresh `Published`).
        (_, Some(_)) => incoming,
        // "Not measured" never erases what is already recorded.
        (_, None) => existing.clone(),
    }
}

/// Monotonic company-status merge (precedence guard).
///
/// A terminal state (`Dissolved` / `InLiquidation`) is never downgraded back to
/// `Active`. Every other transition takes the incoming status, so `Active ->
/// Dissolved` and `InLiquidation -> Dissolved` both apply.
fn merge_status(existing: &EntityStatus, incoming: EntityStatus) -> EntityStatus {
    match (existing, &incoming) {
        (EntityStatus::Dissolved(_) | EntityStatus::InLiquidation(_), EntityStatus::Active) => {
            existing.clone()
        }
        _ => incoming,
    }
}

/// Keep the existing name when the incoming one is blank; otherwise take incoming.
fn pick_name(existing: &str, incoming: String) -> String {
    if incoming.trim().is_empty() {
        existing.to_string()
    } else {
        incoming
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use urban_integrity_schema::types::node::{LocationData, NodeWeight};

    fn entity(name: &str, num: Option<&str>, status: EntityStatus) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: num.map(|s| s.to_string()),
            status,
            incorporation_date: None,
        })
    }

    fn contract_with(provenance: Option<ApplicantProvenance>) -> NodeWeight {
        NodeWeight::Contract(ContractData {
            name: "Erection of warehouse".to_string(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: Some("W05001492".to_string()),
            tender_ref: None,
            application_ref: Some("FPL/2024/001".to_string()),
            application_received_date: None,
            applicant_provenance: provenance,
            portal_detail_handle: None,
        })
    }

    fn merged_provenance(
        existing: Option<ApplicantProvenance>,
        incoming: Option<ApplicantProvenance>,
    ) -> Option<ApplicantProvenance> {
        match merge_node_weight(&contract_with(existing), contract_with(incoming)) {
            NodeWeight::Contract(c) => c.applicant_provenance,
            _ => panic!("expected a Contract weight"),
        }
    }

    /// CR-05: the regression that motivated the precedence guard. A provenance-less source
    /// (Firecrawl fallback, generic-CSS portal) must not be able to downgrade a measured
    /// `Published` — that silently destroyed measured coverage a whole ward at a time.
    #[test]
    fn absent_at_source_never_overwrites_a_measured_published() {
        assert_eq!(
            merged_provenance(
                Some(ApplicantProvenance::Published),
                Some(ApplicantProvenance::AbsentAtSource)
            ),
            Some(ApplicantProvenance::Published)
        );
        assert_eq!(
            merged_provenance(
                Some(ApplicantProvenance::Published),
                Some(ApplicantProvenance::FetchFailed)
            ),
            Some(ApplicantProvenance::Published)
        );
    }

    #[test]
    fn unmeasured_incoming_never_erases_a_stored_claim() {
        assert_eq!(
            merged_provenance(Some(ApplicantProvenance::AbsentAtSource), None),
            Some(ApplicantProvenance::AbsentAtSource)
        );
        assert_eq!(merged_provenance(None, None), None);
    }

    #[test]
    fn an_affirmative_claim_still_wins_over_a_weaker_stored_one() {
        assert_eq!(
            merged_provenance(
                Some(ApplicantProvenance::FetchFailed),
                Some(ApplicantProvenance::Published)
            ),
            Some(ApplicantProvenance::Published)
        );
        assert_eq!(
            merged_provenance(None, Some(ApplicantProvenance::AbsentAtSource)),
            Some(ApplicantProvenance::AbsentAtSource)
        );
    }

    #[test]
    fn active_to_dissolved_applies() {
        let existing = entity("ACME LTD", Some("12345678"), EntityStatus::Active);
        let dt = Utc.with_ymd_and_hms(2025, 4, 15, 0, 0, 0).unwrap();
        let incoming = entity("ACME LTD", None, EntityStatus::Dissolved(dt));
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Entity(e) => {
                assert!(matches!(e.status, EntityStatus::Dissolved(_)));
                // company_number must survive an incoming None.
                assert_eq!(e.company_number.as_deref(), Some("12345678"));
            }
            _ => panic!("expected Entity"),
        }
    }

    #[test]
    fn dissolved_never_downgraded_to_active() {
        let dt = Utc.with_ymd_and_hms(2025, 4, 15, 0, 0, 0).unwrap();
        let existing = entity("ACME LTD", Some("12345678"), EntityStatus::Dissolved(dt));
        let incoming = entity("ACME LTD", Some("12345678"), EntityStatus::Active);
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Entity(e) => assert!(matches!(e.status, EntityStatus::Dissolved(_))),
            _ => panic!("expected Entity"),
        }
    }

    #[test]
    fn inliquidation_to_dissolved_applies() {
        let dt1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let dt2 = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let existing = entity("ACME LTD", None, EntityStatus::InLiquidation(dt1));
        let incoming = entity("ACME LTD", None, EntityStatus::Dissolved(dt2));
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Entity(e) => assert!(matches!(e.status, EntityStatus::Dissolved(_))),
            _ => panic!("expected Entity"),
        }
    }

    #[test]
    fn incoming_company_number_fills_when_absent() {
        let existing = entity("ACME LTD", None, EntityStatus::Active);
        let incoming = entity("ACME LTD", Some("99999999"), EntityStatus::Active);
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Entity(e) => assert_eq!(e.company_number.as_deref(), Some("99999999")),
            _ => panic!("expected Entity"),
        }
    }

    #[test]
    fn blank_incoming_name_keeps_existing() {
        let existing = entity("ACME LTD", None, EntityStatus::Active);
        let incoming = entity("   ", None, EntityStatus::Active);
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Entity(e) => assert_eq!(e.name, "ACME LTD"),
            _ => panic!("expected Entity"),
        }
    }

    #[test]
    fn identical_weight_merges_to_equal() {
        // A no-op re-ingest must produce a weight equal to the existing one so the
        // caller can skip the WAL write (idempotency).
        let existing = entity("ACME LTD", Some("12345678"), EntityStatus::Active);
        let incoming = entity("ACME LTD", Some("12345678"), EntityStatus::Active);
        let merged = merge_node_weight(&existing, incoming);
        assert_eq!(&merged, &existing);
    }

    #[test]
    fn non_entity_variant_takes_incoming() {
        let existing = NodeWeight::Location(LocationData {
            name: "Old".to_string(),
            postcode: None,
            address_line_1: None,
            locality: None,
        });
        let incoming = NodeWeight::Location(LocationData {
            name: "New".to_string(),
            postcode: Some("EC1A1BB".to_string()),
            address_line_1: None,
            locality: None,
        });
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Location(l) => {
                assert_eq!(l.name, "New");
                assert_eq!(l.postcode.as_deref(), Some("EC1A1BB"));
            }
            _ => panic!("expected Location"),
        }
    }

    #[test]
    fn contract_merge_updates_status_preserves_value() {
        // A Contracts Finder refresh advancing award status must apply the new
        // status while preserving fields the refresh omits (value, ocid, ward) —
        // the `incoming.or(existing)` rule, exercising the Contract branch.
        let existing = NodeWeight::Contract(ContractData {
            name: "Build Contract".to_string(),
            ocid: Some("ocds-abc".to_string()),
            value_gbp: Some(50_000.0),
            status: Some("active".to_string()),
            date_published: Some("2025-01-01".to_string()),
            ward_code: Some("E05001234".to_string()),
            tender_ref: None,
            application_ref: Some("FUL/2025/001".to_string()),
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        let incoming = NodeWeight::Contract(ContractData {
            name: "Build Contract".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("complete".to_string()),
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        let merged = merge_node_weight(&existing, incoming);
        match merged {
            NodeWeight::Contract(c) => {
                assert_eq!(
                    c.status.as_deref(),
                    Some("complete"),
                    "incoming status wins"
                );
                assert_eq!(
                    c.value_gbp,
                    Some(50_000.0),
                    "existing value preserved when incoming is None"
                );
                assert_eq!(
                    c.ocid.as_deref(),
                    Some("ocds-abc"),
                    "existing ocid preserved"
                );
                assert_eq!(
                    c.ward_code.as_deref(),
                    Some("E05001234"),
                    "existing ward_code preserved"
                );
                assert_eq!(
                    c.application_ref.as_deref(),
                    Some("FUL/2025/001"),
                    "existing application_ref preserved"
                );
            }
            _ => panic!("expected Contract"),
        }
    }
}
