//! `AlgorithmRequest`/`AlgorithmResult` — the wire contract for `POST /api/graph/algorithm`, the
//! Graph page's tool rail (GRA-01–GRA-04, GRA-06) and every result-list/status-line component
//! that renders what an algorithm run produced.
//!
//! Like every type in this crate, no field carries a serde default attribute — an omitted key is
//! a deserialization error, not a silently-defaulted empty/zero value that looks like real data.
//! `Option` fields that are legitimately absent for a given request/result shape are still
//! required keys on the wire, routed through `required_option` below: an explicit `null` is a
//! deliberate value, an omitted key is a bug.
//!
//! `AlgorithmResult` carries structured counts/entities/truncation data only — never
//! pre-rendered status-line prose. The frontend composes the D-02/D-08/D-09 status-line grammar
//! from these fields in a later plan (the phase 44 UI spec "Status line").

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

/// The caps actually applied to the run that produced an `AlgorithmResult` (post D-08 server-side
/// clamping). Only the caps relevant to the algorithm that produced this result are `Some`; the
/// rest are `None` (never omitted from the wire).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlgorithmCaps {
    #[serde(deserialize_with = "required_option")]
    pub hop_cap: Option<u32>,
    #[serde(deserialize_with = "required_option")]
    pub result_cap: Option<u32>,
    #[serde(deserialize_with = "required_option")]
    pub budget_secs: Option<u32>,
    #[serde(deserialize_with = "required_option")]
    pub k: Option<u32>,
    #[serde(deserialize_with = "required_option")]
    pub top_n: Option<u32>,
    #[serde(deserialize_with = "required_option")]
    pub min_community_size: Option<u32>,
}

/// What the browser posts to `POST /api/graph/algorithm` to run one of the six algorithms
/// (`algorithm` is one of `"all_simple_paths"`, `"shortest_paths"`, `"community"`,
/// `"betweenness"`, `"k_core"`, `"ward_subgraph"`, matching 44-UI-SPEC's rail order).
///
/// `from_id`/`to_id`/`ward_id` are legitimately absent for algorithms that use none of them
/// (community detection, k-core, betweenness) and are wrapped `required_option`. Every other
/// field is always sent by the frontend with the selected algorithm's current default or
/// user-edited value, per 44-UI-SPEC's per-algorithm parameter table, even when irrelevant to the
/// selected algorithm — the server-side dispatcher (a later plan) reads only the fields its
/// algorithm needs and D-08-clamps them. This type enforces no ceiling itself: a view/wire type
/// carries no domain invariant, per this crate's crate-level doc comment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlgorithmRequest {
    pub algorithm: String,
    pub session_id: String,
    pub edge_types_included: Vec<String>,
    pub seed: u64,
    #[serde(deserialize_with = "required_option")]
    pub from_id: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub to_id: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub ward_id: Option<String>,
    /// `highlight_set` only: the raw node ids to show. Always sent (empty for other algorithms).
    ///
    /// No `#[serde(default)]`, per this module's convention — an absent field is a malformed
    /// request, not an empty set, so a caller that forgets it fails loudly instead of silently
    /// highlighting nothing.
    pub ids: Vec<String>,
    /// Restricts the run to this set of node ids, intersected with the server-resolved population
    /// scope — empty means "the whole population scope" (every rail-built request from
    /// `graph_page.rs` sends empty), non-empty means "restrict this run to these ids" (the finding
    /// detail route sends its neighbourhood's own node ids here, so a run posted from
    /// `/finding/:probe` is scoped to exactly the ~9-node neighbourhood the canvas draws, not the
    /// whole analysis population). An id outside the resolved population scope is silently
    /// ignored, not an error — the same "a report one projection old names entities the canvas no
    /// longer has" tolerance `highlight_set` already applies.
    ///
    /// No `#[serde(default)]`, per this module's convention — an absent field is a malformed
    /// request, not an empty (whole-population) scope.
    pub scope_ids: Vec<String>,
    pub hop_cap: u32,
    pub result_cap: u32,
    pub budget_secs: u32,
    pub k: u32,
    pub top_n: u32,
    pub min_community_size: u32,
    pub resolution: f64,
}

/// The mandatory honest-truncation block GRA-01/GRA-06 require on every `AlgorithmResult`.
/// `reason` is `Some("hop_cap")` / `Some("result_cap")` / `Some("wall_clock")` / `Some("sample")`
/// when `truncated`, else `None` — but always a present key (`required_option`), never omitted.
/// `detail` is always present (may be an empty string when not truncated) and is never itself the
/// pre-rendered UI status line — that composition happens in the frontend from this struct's
/// fields plus `AlgorithmResult`'s other fields, per 44-UI-SPEC's status-line grammar table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Truncation {
    pub truncated: bool,
    #[serde(deserialize_with = "required_option")]
    pub reason: Option<String>,
    pub detail: String,
}

/// One hop of a path result: the entity at this hop, and the edge type walked to reach the next
/// hop. `edge_type_to_next` is `None` on the last hop of a path (`required_option` — a present,
/// explicit `null`, never an omitted key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathHop {
    pub entity: EntityRef,
    #[serde(deserialize_with = "required_option")]
    pub edge_type_to_next: Option<String>,
}

/// A single path result — all simple paths, shortest paths and k-shortest paths all render as a
/// `Vec<PathResult>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathResult {
    pub hops: Vec<PathHop>,
}

/// An entity ranked by a score — used for betweenness centrality's top-N rows (`score` is the
/// betweenness value) and k-core's top-nodes-by-degree rows (`score` is the plain degree).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankedEntity {
    pub entity: EntityRef,
    pub score: f64,
}

/// One community detection result row: size, top entities by degree, the canvas hue this
/// community was coloured with (D-15/canvas legend), and the complete member id list.
///
/// `member_node_ids` is sorted ascending by CSR index (the order `seeded_louvain` emits each
/// kept partition in) and is COMPLETE — every kept member of this community, not a capped
/// display subset. It exists so the canvas's automatic "recolour by community" effect and a
/// selected community row's highlight/fit (the phase 44 UI spec Canvas contract; D-12 row selection)
/// can reach every member, not only the top few by degree. `top_entities` stays the separate,
/// capped (currently five), degree-ranked list the results-list row renders — never derive one
/// from the other in either direction; they answer different questions ("what does this row show
/// as a preview" vs. "what does this community actually contain").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommunityResult {
    pub index: u32,
    pub size: u64,
    pub top_entities: Vec<EntityRef>,
    pub color_hue_deg: u32,
    pub member_node_ids: Vec<String>,
}

/// The per-algorithm result shape, one variant per algorithm identifier (internally tagged on
/// `"algorithm"`, matching `AlgorithmRequest.algorithm`'s seven string values).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "algorithm")]
pub enum AlgorithmOutcome {
    #[serde(rename = "all_simple_paths")]
    Paths { paths: Vec<PathResult> },
    #[serde(rename = "shortest_paths")]
    ShortestPaths {
        paths: Vec<PathResult>,
        requested_k: u32,
    },
    #[serde(rename = "community")]
    Community {
        communities: Vec<CommunityResult>,
        hidden_node_ids: Vec<String>,
    },
    #[serde(rename = "k_core")]
    KCore {
        k: u32,
        node_count: u64,
        edge_count: u64,
        top_nodes: Vec<RankedEntity>,
        hidden_node_ids: Vec<String>,
    },
    #[serde(rename = "betweenness")]
    Betweenness {
        sampled_sources: u64,
        total_sources: u64,
        top_nodes: Vec<RankedEntity>,
    },
    #[serde(rename = "ward_subgraph")]
    WardSubgraph {
        ward: EntityRef,
        hops: u32,
        node_count: u64,
        edge_count: u64,
        node_ids: Vec<String>,
    },
    /// A deep link's entity set, resolved against the canonical population. `unresolved_count`
    /// is how many of the requested ids are NOT in the population — reported rather than
    /// dropped, because a link that silently shows fewer entities than it names is the failure
    /// this variant exists to make visible.
    #[serde(rename = "highlight_set")]
    HighlightSet {
        node_ids: Vec<String>,
        unresolved_count: u64,
    },
}

/// What `POST /api/graph/algorithm` returns: the per-algorithm outcome plus every field the
/// D-02/D-08/D-09 status-line grammar needs to compose its text, and the D-05 cache-key fields
/// (`seed_used`, `excluded_edge_types`) round-tripped exactly so a cached response is
/// indistinguishable from a fresh one.
///
/// `forced_included_edge_type` is `Some("Represents")` only for `WardSubgraph` (D-01's "always
/// walks Represents… says so in its rail text"); `seed_used` is `Some(n)` only for
/// `Community`/`Betweenness` (D-10). Both are `required_option`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlgorithmResult {
    pub outcome: AlgorithmOutcome,
    pub population_generated_at: String,
    pub excluded_edge_types: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub forced_included_edge_type: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub seed_used: Option<u64>,
    pub caps_used: AlgorithmCaps,
    pub truncation: Truncation,
    pub wall_clock_ms: u64,
}

#[cfg(test)]
mod tests {
    /// The wire tag must be `highlight_set`, and `unresolved_count` must survive the round trip —
    /// a deep link that silently loses the "N of these are outside the population" figure is a
    /// link that lies about what it is showing.
    #[test]
    fn highlight_set_round_trips_with_its_algorithm_tag() {
        let outcome = AlgorithmOutcome::HighlightSet {
            node_ids: vec!["a".to_string()],
            unresolved_count: 1,
        };
        let json = serde_json::to_value(&outcome).expect("serialise");
        assert_eq!(json["algorithm"], "highlight_set");
        assert_eq!(json["unresolved_count"], 1);

        let back: AlgorithmOutcome = serde_json::from_value(json).expect("deserialise");
        assert_eq!(back, outcome);
    }

    /// `ids` has no `#[serde(default)]`, so an absent field is a hard error rather than an empty
    /// set — the same absent-vs-empty discipline the rest of this module keeps.
    #[test]
    fn a_request_without_ids_is_rejected_not_defaulted() {
        let mut value = serde_json::to_value(sample_request()).expect("serialise");
        value.as_object_mut().unwrap().remove("ids");
        let parsed: Result<AlgorithmRequest, _> = serde_json::from_value(value);
        assert!(
            parsed.is_err(),
            "absent `ids` must not silently become empty"
        );
    }

    use super::*;

    fn sample_request() -> AlgorithmRequest {
        AlgorithmRequest {
            algorithm: "all_simple_paths".to_string(),
            session_id: "session-abc".to_string(),
            edge_types_included: vec!["Directs".to_string(), "AwardedTo".to_string()],
            seed: 42,
            from_id: Some("director:jane-smith".to_string()),
            to_id: Some("GB-COH:99000001".to_string()),
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        }
    }

    /// `scope_ids` has no `#[serde(default)]`, so an absent field is a hard error rather than an
    /// empty (whole-population) scope — the same absent-vs-empty discipline `ids` keeps.
    #[test]
    fn a_request_without_scope_ids_is_rejected_not_defaulted() {
        let mut value = serde_json::to_value(sample_request()).expect("serialise");
        value.as_object_mut().unwrap().remove("scope_ids");
        let parsed: Result<AlgorithmRequest, _> = serde_json::from_value(value);
        assert!(
            parsed.is_err(),
            "absent `scope_ids` must not silently become the whole population"
        );
    }

    /// `AlgorithmRequest` round-trips through `serde_json::to_string`/`from_str`.
    #[test]
    fn algorithm_request_round_trips() {
        let request = sample_request();
        let json = serde_json::to_string(&request).expect("AlgorithmRequest must serialize");
        let back: AlgorithmRequest =
            serde_json::from_str(&json).expect("AlgorithmRequest must deserialize");
        assert_eq!(back, request);
    }

    /// An `AlgorithmRequest` JSON body missing the `session_id` key fails to deserialize (no
    /// serde default attribute on that field).
    #[test]
    fn missing_session_id_key_fails_to_deserialize() {
        let missing_session_id = r#"{
            "algorithm":"all_simple_paths","edge_types_included":["Directs"],"seed":42,
            "from_id":"director:jane-smith","to_id":"GB-COH:99000001","ward_id":null,
            "hop_cap":4,"result_cap":100,"budget_secs":5,"k":3,"top_n":50,
            "min_community_size":5,"resolution":1.0
        }"#;
        assert!(
            serde_json::from_str::<AlgorithmRequest>(missing_session_id).is_err(),
            "an omitted `session_id` key must be a deserialization error, not a silent default"
        );
    }

    fn sample_truncation(truncated: bool) -> Truncation {
        if truncated {
            Truncation {
                truncated: true,
                reason: Some("hop_cap".to_string()),
                detail: "stopped at hop cap 4".to_string(),
            }
        } else {
            Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            }
        }
    }

    fn sample_caps() -> AlgorithmCaps {
        AlgorithmCaps {
            hop_cap: Some(4),
            result_cap: Some(100),
            budget_secs: Some(5),
            k: None,
            top_n: None,
            min_community_size: None,
        }
    }

    fn sample_entity(id: &str, name: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Entity".to_string(),
        }
    }

    /// `AlgorithmOutcome::Paths { .. }` serializes with `"algorithm":"all_simple_paths"` as the
    /// tag (internally-tagged enum) and round-trips.
    #[test]
    fn paths_outcome_tags_as_all_simple_paths_and_round_trips() {
        let outcome = AlgorithmOutcome::Paths {
            paths: vec![PathResult {
                hops: vec![
                    PathHop {
                        entity: sample_entity("director:jane-smith", "Jane Smith"),
                        edge_type_to_next: Some("AwardedTo".to_string()),
                    },
                    PathHop {
                        entity: sample_entity("GB-COH:99000001", "Acme Ltd"),
                        edge_type_to_next: None,
                    },
                ],
            }],
        };
        let json = serde_json::to_string(&outcome).expect("AlgorithmOutcome must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["algorithm"], serde_json::json!("all_simple_paths"));

        let back: AlgorithmOutcome =
            serde_json::from_str(&json).expect("AlgorithmOutcome must deserialize");
        assert_eq!(back, outcome);
    }

    /// `AlgorithmOutcome::WardSubgraph { .. }` serializes with `"algorithm":"ward_subgraph"` and
    /// round-trips.
    #[test]
    fn ward_subgraph_outcome_tags_as_ward_subgraph_and_round_trips() {
        let outcome = AlgorithmOutcome::WardSubgraph {
            ward: sample_entity("ward:X05001499", "Sample Ward North"),
            hops: 2,
            node_count: 1_204,
            edge_count: 3_880,
            node_ids: vec!["ward:X05001499".to_string(), "GB-COH:99000001".to_string()],
        };
        let json = serde_json::to_string(&outcome).expect("AlgorithmOutcome must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["algorithm"], serde_json::json!("ward_subgraph"));

        let back: AlgorithmOutcome =
            serde_json::from_str(&json).expect("AlgorithmOutcome must deserialize");
        assert_eq!(back, outcome);
    }

    /// `AlgorithmOutcome::Community { .. }` serializes with `"algorithm":"community"` as the tag
    /// and round-trips; `member_node_ids` carries the complete membership while `top_entities`
    /// stays a capped subset of it — the two lists are never required to be equal length.
    #[test]
    fn community_outcome_tags_as_community_and_round_trips() {
        let outcome = AlgorithmOutcome::Community {
            communities: vec![CommunityResult {
                index: 0,
                size: 3,
                top_entities: vec![sample_entity("director:jane-smith", "Jane Smith")],
                color_hue_deg: 0,
                member_node_ids: vec![
                    "director:jane-smith".to_string(),
                    "GB-COH:99000001".to_string(),
                    "director:john-doe".to_string(),
                ],
            }],
            hidden_node_ids: vec!["director:jack-jones".to_string()],
        };
        let json = serde_json::to_string(&outcome).expect("AlgorithmOutcome must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["algorithm"], serde_json::json!("community"));
        assert_eq!(
            value["communities"][0]["member_node_ids"]
                .as_array()
                .map(|a| a.len()),
            Some(3)
        );

        let back: AlgorithmOutcome =
            serde_json::from_str(&json).expect("AlgorithmOutcome must deserialize");
        assert_eq!(back, outcome);
    }

    /// A `CommunityResult` JSON body missing the `member_node_ids` key fails to deserialize (no
    /// serde default attribute on that field — an omitted key is a bug, not an empty list).
    #[test]
    fn community_result_missing_member_node_ids_key_fails_to_deserialize() {
        let missing_member_node_ids = r#"{
            "index":0,"size":3,"top_entities":[],"color_hue_deg":0
        }"#;
        assert!(
            serde_json::from_str::<CommunityResult>(missing_member_node_ids).is_err(),
            "an omitted `member_node_ids` key must be a deserialization error, not a silent empty vec"
        );
    }

    /// A `Truncation` JSON body missing the `reason` key fails to deserialize (required_option —
    /// `null` is a valid value, an omitted key is not).
    #[test]
    fn truncation_missing_reason_key_fails_to_deserialize() {
        let missing_reason = r#"{"truncated":false,"detail":""}"#;
        assert!(
            serde_json::from_str::<Truncation>(missing_reason).is_err(),
            "an omitted `reason` key must be a deserialization error, not a silent None"
        );
    }

    /// An `AlgorithmResult` with `truncation.truncated == false` still requires
    /// `truncation.reason` to be present and `null` (never an omitted field).
    #[test]
    fn untruncated_result_still_requires_present_null_reason() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::KCore {
                k: 3,
                node_count: 1_204,
                edge_count: 3_880,
                top_nodes: vec![RankedEntity {
                    entity: sample_entity("director:jane-smith", "Jane Smith"),
                    score: 12.0,
                }],
                hidden_node_ids: vec!["director:john-doe".to_string()],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: sample_caps(),
            truncation: sample_truncation(false),
            wall_clock_ms: 812,
        };
        let json = serde_json::to_string(&result).expect("AlgorithmResult must serialize");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["truncation"]["reason"].is_null());
        assert!(
            value["truncation"]
                .as_object()
                .unwrap()
                .contains_key("reason"),
            "reason must be a present key even when null"
        );

        let back: AlgorithmResult =
            serde_json::from_str(&json).expect("AlgorithmResult must deserialize");
        assert_eq!(back, result);

        let missing_reason_in_result = json.replacen(r#","reason":null"#, "", 1);
        assert!(
            serde_json::from_str::<AlgorithmResult>(&missing_reason_in_result).is_err(),
            "an omitted nested `reason` key must be a deserialization error"
        );
    }
}
