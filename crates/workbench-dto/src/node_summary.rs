//! `NodeSummary` — the Graph page's side card payload, returned when the operator clicks a node
//! on the canvas (the phase 44 UI spec "Node click → side card": name, kind, degree, earliest/latest
//! edge).
//!
//! Like every type in this crate, no field carries a serde default attribute — an omitted key is
//! a deserialization error, not a silently-defaulted empty/zero value that looks like real data.

use serde::{Deserialize, Serialize};

use crate::entity_ref::EntityRef;

/// Copied verbatim from `population_meta::required_option` (see that module's doc comment for
/// the full rationale) — this crate's established convention is each DTO module owns its own
/// copy, never a shared import.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// The side card's node-click payload: the entity, its degree, and the earliest/latest edge
/// timestamps across the node's edges. `earliest_edge`/`latest_edge` are RFC3339 strings when the
/// node has at least one edge, `None` for a genuinely isolated node — never a fabricated or
/// duplicated timestamp. Both are `required_option`: a present, explicit `null`, never an omitted
/// key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeSummary {
    pub entity: EntityRef,
    pub degree: u64,
    #[serde(deserialize_with = "required_option")]
    pub earliest_edge: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub latest_edge: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entity() -> EntityRef {
        EntityRef {
            id: "director:jane-smith".to_string(),
            name: "Jane Smith".to_string(),
            kind: "Entity".to_string(),
        }
    }

    /// `NodeSummary` round-trips through JSON.
    #[test]
    fn node_summary_round_trips() {
        let summary = NodeSummary {
            entity: sample_entity(),
            degree: 12,
            earliest_edge: Some("2020-03-01T00:00:00Z".to_string()),
            latest_edge: Some("2026-01-15T00:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&summary).expect("NodeSummary must serialize");
        let back: NodeSummary = serde_json::from_str(&json).expect("NodeSummary must deserialize");
        assert_eq!(back, summary);
    }

    /// A `NodeSummary` JSON body with `earliest_edge`/`latest_edge` both `null` (an isolated node
    /// with no edges) round-trips and is distinct from a body where the keys are simply absent
    /// (which must fail to deserialize).
    #[test]
    fn isolated_node_round_trips_with_null_edges_but_omitted_keys_fail() {
        let isolated = NodeSummary {
            entity: sample_entity(),
            degree: 0,
            earliest_edge: None,
            latest_edge: None,
        };
        let json = serde_json::to_string(&isolated).expect("NodeSummary must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["earliest_edge"].is_null());
        assert!(value["latest_edge"].is_null());
        assert!(
            value.as_object().unwrap().contains_key("earliest_edge"),
            "earliest_edge must be a present key even when null"
        );
        assert!(
            value.as_object().unwrap().contains_key("latest_edge"),
            "latest_edge must be a present key even when null"
        );

        let back: NodeSummary = serde_json::from_str(&json).expect("NodeSummary must deserialize");
        assert_eq!(back, isolated);

        let missing_earliest_edge = r#"{
            "entity":{"id":"director:jane-smith","name":"Jane Smith","kind":"Entity"},
            "degree":0,"latest_edge":null
        }"#;
        assert!(
            serde_json::from_str::<NodeSummary>(missing_earliest_edge).is_err(),
            "an omitted `earliest_edge` key must be a deserialization error, not a silent None"
        );

        let missing_latest_edge = r#"{
            "entity":{"id":"director:jane-smith","name":"Jane Smith","kind":"Entity"},
            "degree":0,"earliest_edge":null
        }"#;
        assert!(
            serde_json::from_str::<NodeSummary>(missing_latest_edge).is_err(),
            "an omitted `latest_edge` key must be a deserialization error, not a silent None"
        );
    }
}
