//! Reading an entity's status out of a `graph_events` payload.
//!
//! `weight_json` holds a serialised [`NodeWeight`], which is `#[serde(tag = "type")]`:
//!
//! ```json
//! {"type":"Entity","name":"ACME LTD","status":{"type":"Dissolved","content":"2026-07-28T00:00:00Z"}}
//! ```
//!
//! Two detectors hand-rolled a path into that payload as `["data"]["status"]["type"]` and
//! fell through to `.unwrap_or("Active")`. There is no `data` key and never has been, so
//! **every** entity read as Active: `property_accumulation.rs`'s D-11 gate ("skip
//! InLiquidation / Dissolved") never skipped anything, and `evasion.rs` treated every
//! officer as active. Measured live, 0 of 25,170 terminal-status payloads contain a `data`
//! key.
//!
//! The fixtures hid it. `make_entity_node` in the property-accumulation test built
//! `{"type":"Entity","data":{"status":{...}}}` by hand — the same shape the parser expected
//! and the same shape the database has never held — so the D-11 negative test passed against
//! a payload that cannot occur. Deserialise the real type and let the compiler own the shape.

use urban_integrity_schema::types::node::{EntityStatus, NodeWeight};

/// Whether the node described by `weight_json` is an active entity.
///
/// - `Some(true)` / `Some(false)` — an `Entity` node, active or not.
/// - `None` — not an entity, or unparseable. Officers carry no status at all
///   (`OfficerData` has no such field), so the caller decides what an absent status means;
///   both current callers treat it as active, which is the pre-existing default.
pub fn entity_is_active(weight_json: &str) -> Option<bool> {
    match serde_json::from_str::<NodeWeight>(weight_json) {
        Ok(NodeWeight::Entity(data)) => Some(matches!(data.status, EntityStatus::Active)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::node::{EntityData, OfficerData};

    fn entity(status: EntityStatus) -> String {
        serde_json::to_string(&NodeWeight::Entity(EntityData {
            name: "ACME LTD".to_string(),
            company_number: Some("00133804".to_string()),
            status,
            incorporation_date: None,
        }))
        .unwrap()
    }

    #[test]
    fn an_active_entity_is_active() {
        assert_eq!(entity_is_active(&entity(EntityStatus::Active)), Some(true));
    }

    #[test]
    fn a_dissolved_entity_is_not_active() {
        assert_eq!(
            entity_is_active(&entity(EntityStatus::Dissolved(chrono::Utc::now()))),
            Some(false),
            "the D-11 gate exists to skip these; reading them as Active disables it"
        );
    }

    #[test]
    fn an_in_liquidation_entity_is_not_active() {
        assert_eq!(
            entity_is_active(&entity(EntityStatus::InLiquidation(chrono::Utc::now()))),
            Some(false)
        );
    }

    #[test]
    fn the_real_serialised_shape_has_no_data_key() {
        // The regression itself: the payload the parser used to look for cannot occur.
        let json = entity(EntityStatus::Dissolved(chrono::Utc::now()));
        let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            raw.get("data").is_none(),
            "NodeWeight is serde(tag=\"type\") — a `data` wrapper would mean the schema moved: {json}"
        );
        assert_eq!(raw["status"]["type"].as_str(), Some("Dissolved"));
        assert!(
            raw["status"]["content"].is_string(),
            "the date is under `content`, not `date` — evasion.rs's comment said `date`"
        );
    }

    #[test]
    fn a_node_with_no_status_concept_returns_none() {
        let officer = serde_json::to_string(&NodeWeight::Officer(OfficerData {
            name: "Jane Smith".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: None,
        }))
        .unwrap();
        assert_eq!(
            entity_is_active(&officer),
            None,
            "OfficerData has no status field — the caller must choose the default"
        );
    }

    #[test]
    fn junk_returns_none_rather_than_guessing() {
        assert_eq!(entity_is_active("not json"), None);
        assert_eq!(entity_is_active("{}"), None);
        assert_eq!(entity_is_active(r#"{"type":"Nonsense"}"#), None);
    }
}
