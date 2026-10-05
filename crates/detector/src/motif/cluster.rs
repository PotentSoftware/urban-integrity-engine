use std::collections::{HashMap, HashSet};

use petgraph::visit::EdgeRef;
use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_schema::types::edge::EdgeWeight;

use crate::alert::result::MotifResult;

/// Formation-agent / virtual-office postcode list, embedded from the version-controlled
/// data file so the list is a single reviewable source of truth rather than a literal in
/// this module. See `data/formation_agent_postcodes.txt` for the curation policy.
const FORMATION_AGENT_POSTCODES_RAW: &str =
    include_str!("../../../../data/formation_agent_postcodes.txt");

/// Parse a postcode list file: one entry per line, `#` starts a comment, whitespace and
/// case normalised (uppercased, spaces/tabs stripped) so "wc2h 9jq" == "WC2H9JQ".
fn parse_postcode_list(raw: &str) -> HashSet<String> {
    raw.lines()
        .filter_map(|line| {
            let code = line.split('#').next().unwrap_or("").trim();
            if code.is_empty() {
                return None;
            }
            Some(code.to_ascii_uppercase().replace([' ', '\t'], ""))
        })
        .collect()
}

/// Synthetic entity ids that stand in for "we don't know who this is" and must never be
/// counted as a co-located bidder.
///
/// `director:unknown-applicant` is minted by `map_planning_item` for every planning record
/// whose portal published no applicant name. It is ONE node aggregating thousands of
/// records across the whole corpus, so it is co-located with almost every address and joins
/// almost every cluster — padding participant lists and, at `min_bidders = 3`, manufacturing
/// clusters outright.
///
/// Measured on `an_anglesey_aebaa785` after the 2026-07-28 detection run: of 27,044
/// planning-sourced AddressCluster alerts, **71.5% had only two real participants** and fired
/// solely because the placeholder padded them to three. Excluding it is not a tuning
/// preference — those alerts do not meet the detector's own threshold.
///
/// This is the same defect class as the buyer and formation-agent suppressions below: an
/// entity that is structurally co-located everywhere is not evidence of anything.
const PLACEHOLDER_ENTITY_IDS: &[&str] = &["director:unknown-applicant"];

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub min_bidders: usize,
    /// Normalised postcodes to suppress (formation agents / virtual offices). Defaults to
    /// the embedded `data/formation_agent_postcodes.txt`; tests may override.
    pub formation_agent_postcodes: HashSet<String>,
    /// Entity ids that are placeholders rather than real parties. See
    /// [`PLACEHOLDER_ENTITY_IDS`].
    pub placeholder_entity_ids: HashSet<String>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            min_bidders: 3,
            formation_agent_postcodes: parse_postcode_list(FORMATION_AGENT_POSTCODES_RAW),
            placeholder_entity_ids: PLACEHOLDER_ENTITY_IDS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        }
    }
}

/// Extract the normalised postcode from a `LOC:{postcode}:{house}` id.
fn postcode_of_loc_id(loc_id: &str) -> Option<&str> {
    loc_id
        .strip_prefix("LOC:")
        .and_then(|rest| rest.split(':').next())
}

/// True if a Location's name is the Companies House default-address placeholder. The
/// street string is literally "<company-number> - COMPANIES HOUSE DEFAULT ADDRESS" and
/// appears under every jurisdiction's registrar postcode (CF14 8LH, BT1, EH7, ...), so
/// match on the name marker rather than any single postcode.
fn is_ch_default_address(name: &str) -> bool {
    name.to_ascii_uppercase()
        .contains("COMPANIES HOUSE DEFAULT ADDRESS")
}

pub struct ClusterDetector {
    pub config: ClusterConfig,
}

impl ClusterDetector {
    pub fn new(config: ClusterConfig) -> Self {
        Self { config }
    }

    /// Finds `Location` nodes shared by >= `min_bidders` distinct **bidder** Entities.
    ///
    /// Edges run Entity → Location (LocatedAt direction), so this iterates every
    /// Entity node and indexes its Location neighbors. Grouping by location index
    /// then finds shared addresses.
    ///
    /// Contracting **authorities** (buyers) are excluded: an entity with any outgoing
    /// `IssuedBy` edge issues contracts rather than bidding for them, so its address is
    /// a public office, not a shell-company registration. Without this filter, dozens of
    /// councils/NHS bodies keyed `CF-PARTY:GB-CFS-*` and co-located in shared government
    /// buildings (e.g. "Hinton House") fired high-score clusters that are pure noise.
    pub fn check(&self, snapshot: &CsrSnapshot, id_map: &HashMap<u32, String>) -> Vec<MotifResult> {
        use std::collections::HashMap as HMap;

        // location_node_idx → list of distinct entity IDs that point to it
        let mut loc_entities: HMap<u32, Vec<String>> = HMap::new();

        // contract_node_idx → applicant entity id, built from every Entity's own outgoing
        // AwardedTo edges (entity -> contract). The CSR graph is outgoing-only with no
        // incoming-edge index, so this reverse lookup must be assembled while iterating
        // entities' own edges — there is no other way to answer "who was awarded contract C".
        let mut contract_awarded_by: HMap<u32, String> = HMap::new();

        for node_idx in 0..snapshot.graph.node_count() {
            if !matches!(
                &snapshot.graph[node_idx as u32],
                urban_integrity_schema::types::node::NodeWeight::Entity(_)
            ) {
                continue;
            }
            // Skip contracting authorities — they procure, they don't bid.
            if entity_is_buyer(snapshot, node_idx as u32) {
                continue;
            }
            let Some(entity_id) = id_map.get(&(node_idx as u32)) else {
                continue;
            };
            // Skip placeholders — they stand for an unknown party, not a co-located one.
            if self.config.placeholder_entity_ids.contains(entity_id) {
                continue;
            }
            for &neighbor in snapshot.graph.neighbors_slice(node_idx as u32) {
                if matches!(
                    &snapshot.graph[neighbor],
                    urban_integrity_schema::types::node::NodeWeight::Location(_)
                ) {
                    loc_entities
                        .entry(neighbor)
                        .or_default()
                        .push(entity_id.clone());
                }
            }
            // Record this entity as the applicant for every contract it was awarded, so the
            // Contract -> LocatedAt -> Location path below (post-D-04: planning `LocatedAt`
            // edges are sourced from the Contract node, not the applicant Entity) can attribute
            // back to the real party.
            for edge in snapshot.graph.edges(node_idx as u32) {
                if matches!(edge.weight(), EdgeWeight::AwardedTo { .. }) {
                    contract_awarded_by.insert(edge.target(), entity_id.clone());
                }
            }
        }

        // Second pass: Contract -> LocatedAt -> Location, attributed to the applicant via the
        // reverse AwardedTo lookup built above. Deliberately NOT gated by `entity_is_buyer` —
        // planning applicants are never contracting authorities in this domain, so that check
        // does not apply on this path.
        for node_idx in 0..snapshot.graph.node_count() {
            if !matches!(
                &snapshot.graph[node_idx as u32],
                urban_integrity_schema::types::node::NodeWeight::Contract(_)
            ) {
                continue;
            }
            let Some(applicant_id) = contract_awarded_by.get(&(node_idx as u32)) else {
                continue;
            };
            // Skip placeholders on this path too — mirrors the entity-path exclusion.
            if self.config.placeholder_entity_ids.contains(applicant_id) {
                continue;
            }
            for edge in snapshot.graph.edges(node_idx as u32) {
                if !matches!(edge.weight(), EdgeWeight::LocatedAt { .. }) {
                    continue;
                }
                let neighbor = edge.target();
                if matches!(
                    &snapshot.graph[neighbor],
                    urban_integrity_schema::types::node::NodeWeight::Location(_)
                ) {
                    loc_entities
                        .entry(neighbor)
                        .or_default()
                        .push(applicant_id.clone());
                }
            }
        }

        let mut results = Vec::new();

        for (loc_idx, mut entity_ids) in loc_entities {
            entity_ids.sort();
            entity_ids.dedup();
            if entity_ids.len() < self.config.min_bidders {
                continue;
            }
            let Some(loc_id) = id_map.get(&loc_idx) else {
                continue;
            };
            let loc_weight = match &snapshot.graph[loc_idx] {
                urban_integrity_schema::types::node::NodeWeight::Location(l) => l,
                _ => continue,
            };
            // Suppress known non-signal addresses: formation-agent / virtual-office
            // buildings, and the Companies House default-address placeholder. Co-location
            // at these is not a shell-company indicator.
            let at_formation_agent = postcode_of_loc_id(loc_id)
                .is_some_and(|pc| self.config.formation_agent_postcodes.contains(pc));
            if at_formation_agent || is_ch_default_address(&loc_weight.name) {
                continue;
            }
            results.push(MotifResult {
                motif_type: crate::alert::result::MotifType::AddressCluster,
                unique_hash: crate::alert::result::motif_hash(&[loc_id]),
                participants: entity_ids.clone(),
                risk_score: 0.7 + (entity_ids.len() as f64 * 0.05).min(0.25),
                evidence_snapshot: format!(
                    "Address cluster detected: {} entities registered at {}",
                    entity_ids.len(),
                    loc_weight.name
                ),
                detected_at_event: snapshot.baked_at_event,
                version: 1,
                last_seen_at: chrono::Utc::now(),
                score_breakdown: crate::alert::result::ScoreBreakdown {
                    s_phoenix: 0.0,
                    s_proximity: 0.0,
                    s_clustering: 0.7,
                    p_base: 0.7,
                    m_env: 1.0,
                    p_final: 0.7,
                    imd_rank: None,
                    ward_code: None,
                },
                withdrawn_at: None,
                withdrawn_reason: None,
            });
        }

        results
    }
}

/// True if the entity acts as a contracting authority — has any outgoing `IssuedBy`
/// edge. Such entities issue contracts (buyers) and must not be counted as bidders in
/// address clusters.
fn entity_is_buyer(snapshot: &CsrSnapshot, entity_idx: u32) -> bool {
    snapshot
        .graph
        .edges(entity_idx)
        .any(|e| matches!(e.weight(), EdgeWeight::IssuedBy { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, LocatedAtData};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{
        EntityData, EntityStatus, LocationData, NodeWeight, OfficerData,
    };

    fn make_located_at_edge() -> EdgeWeight {
        EdgeWeight::LocatedAt {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: LocatedAtData {
                details: "registered address".into(),
            },
        }
    }

    fn make_directs_edge() -> EdgeWeight {
        use urban_integrity_schema::types::edge::DirectsData;
        EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "director".into(),
                officer_role: None,
            },
        }
    }

    fn make_meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    fn make_issued_by_edge() -> EdgeWeight {
        use urban_integrity_schema::types::edge::IssuedByData;
        EdgeWeight::IssuedBy {
            meta: make_meta(),
            data: IssuedByData {
                details: "buyer".into(),
            },
        }
    }

    fn make_awarded_to_edge() -> EdgeWeight {
        use urban_integrity_schema::types::edge::AwardedToData;
        EdgeWeight::AwardedTo {
            meta: make_meta(),
            data: AwardedToData {
                details: "award:x".into(),
            },
        }
    }

    fn make_contract(name: &str) -> NodeWeight {
        use urban_integrity_schema::types::node::ContractData;
        NodeWeight::Contract(ContractData {
            name: name.into(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        })
    }

    fn make_entity_at(storage: &GraphStorage, id: &EntityId, name: &str, loc_id: &EntityId) {
        storage.insert_node(
            id.clone(),
            NodeWeight::Entity(EntityData {
                name: name.into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage
            .insert_edge(id, loc_id, make_located_at_edge())
            .unwrap();
    }

    fn make_id_map(storage: &GraphStorage) -> HashMap<u32, String> {
        let mut m = HashMap::new();
        for (id, idx, _, _) in storage.snapshot_nodes() {
            m.insert(idx.index() as u32, id.0.clone());
        }
        m
    }

    #[test]
    fn test_cluster_detector_creation() {
        let config = ClusterConfig::default();
        let detector = ClusterDetector::new(config);
        assert_eq!(detector.config.min_bidders, 3);
    }

    #[test]
    fn test_cluster_check_empty() {
        let storage = GraphStorage::new();
        let snapshot = bake_csr(&storage, 1);
        let id_map = HashMap::new();
        let detector = ClusterDetector::new(ClusterConfig::default());
        let results = detector.check(&snapshot, &id_map);
        assert!(results.is_empty());
    }

    #[test]
    fn test_cluster_detects_three_entities_at_one_address() {
        // Edges: Entity → Location (production direction from planning mapper)
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:001".into());
        let e1 = EntityId("ENT:C1".into());
        let e2 = EntityId("ENT:C2".into());
        let e3 = EntityId("ENT:C3".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "1 Shell Street, London".into(),
                postcode: Some("EC1A1BB".into()),
                address_line_1: None,
                locality: None,
            }),
        );
        for (id, name) in [(&e1, "Alpha Ltd"), (&e2, "Beta Ltd"), (&e3, "Gamma Ltd")] {
            storage.insert_node(
                id.clone(),
                NodeWeight::Entity(EntityData {
                    name: name.into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage
                .insert_edge(id, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 3);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(
            results.len(),
            1,
            "Expected 1 cluster alert for 3 entities at one address"
        );
        assert_eq!(
            results[0].motif_type,
            crate::alert::result::MotifType::AddressCluster
        );
        assert_eq!(results[0].participants.len(), 3);
        assert!(results[0].risk_score >= 0.7);
    }

    /// Criterion-5 fixture evidence for Phase 24 IDN-05: proves AddressCluster fires
    /// on exactly the shape of data planning applicants produce — 3+ distinct
    /// `director:{slug}` Entity nodes sharing one Location. The address used here
    /// ("45-49 Market Street, Holyhead") is a real live cluster documented in
    /// the phase 24 research notes' Live Verification table (10 distinct applicants found
    /// live), so this fixture's provenance traces to that evidence.
    #[test]
    fn test_cluster_detects_three_planning_applicants_at_one_address() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:HOLYHEAD001".into());
        let d1 = EntityId("director:alpha-developments".into());
        let d2 = EntityId("director:beta-holdings".into());
        let d3 = EntityId("director:gamma-construction".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "45-49 Market Street, Holyhead".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        for (id, name) in [
            (&d1, "Alpha Developments"),
            (&d2, "Beta Holdings"),
            (&d3, "Gamma Construction"),
        ] {
            storage.insert_node(
                id.clone(),
                NodeWeight::Entity(EntityData {
                    name: name.into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage
                .insert_edge(id, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 3);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(
            results.len(),
            1,
            "Expected 1 cluster alert for 3 director:{{slug}} planning applicants at one address"
        );
        assert_eq!(
            results[0].motif_type,
            crate::alert::result::MotifType::AddressCluster
        );
        assert_eq!(results[0].participants.len(), 3);
        assert!(results[0].risk_score >= 0.7);
    }

    /// The `director:unknown-applicant` placeholder must never count as a co-located party.
    ///
    /// It is one synthetic node standing in for every planning record whose portal published
    /// no applicant name, so it is co-located with almost every address in the corpus.
    /// Counting it padded participant lists and, at `min_bidders = 3`, manufactured clusters
    /// from pairs: measured on `an_anglesey_aebaa785`, 71.5% of the 27,044 planning-sourced
    /// AddressCluster alerts had only two real participants and fired solely on that padding.
    #[test]
    fn test_placeholder_applicant_is_not_counted_as_a_bidder() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:LLANEILIAN001".into());
        let d1 = EntityId("director:a-example-applicant".into());
        let d2 = EntityId("director:jordan-sample".into());
        let placeholder = EntityId("director:unknown-applicant".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "Example Farm, Llaneilian".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        for (id, name) in [
            (&d1, "A Example Applicant"),
            (&d2, "Jordan Sample"),
            (&placeholder, "Unknown Applicant"),
        ] {
            storage.insert_node(
                id.clone(),
                NodeWeight::Entity(EntityData {
                    name: name.into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage
                .insert_edge(id, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 3);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        // This mirrors a real alert shape from the 2026-07-28 run: two genuine applicants
        // plus the placeholder. With the placeholder excluded only two real parties remain,
        // which is below min_bidders — so it must not fire at all.
        assert!(
            results.is_empty(),
            "two real applicants padded to three by the placeholder must not raise a cluster; \
             got {results:?}"
        );
    }

    /// The exclusion must not suppress a genuine cluster that merely happens to include the
    /// placeholder — three real applicants still cluster.
    #[test]
    fn test_placeholder_does_not_suppress_a_genuine_cluster() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:HOLYHEAD002".into());
        let reals = [
            (EntityId("director:alpha-developments".into()), "Alpha"),
            (EntityId("director:beta-holdings".into()), "Beta"),
            (EntityId("director:gamma-construction".into()), "Gamma"),
        ];
        let placeholder = EntityId("director:unknown-applicant".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "45-49 Market Street, Holyhead".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        for (id, name) in reals
            .iter()
            .map(|(i, n)| (i, *n))
            .chain(std::iter::once((&placeholder, "Unknown Applicant")))
        {
            storage.insert_node(
                id.clone(),
                NodeWeight::Entity(EntityData {
                    name: name.into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage
                .insert_edge(id, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 4);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(results.len(), 1, "three real applicants still cluster");
        assert_eq!(
            results[0].participants.len(),
            3,
            "the placeholder must not appear in the participant list"
        );
        assert!(
            !results[0]
                .participants
                .iter()
                .any(|p| p == "director:unknown-applicant"),
            "placeholder leaked into participants: {:?}",
            results[0].participants
        );
    }

    #[test]
    fn test_cluster_below_threshold_no_alert() {
        // Only 2 entities at address with min_bidders=3 — no alert
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:T1".into());
        let e1 = EntityId("ENT:T1".into());
        let e2 = EntityId("ENT:T2".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "3 Below Ave".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        for (id, name) in [(&e1, "X Ltd"), (&e2, "Y Ltd")] {
            storage.insert_node(
                id.clone(),
                NodeWeight::Entity(EntityData {
                    name: name.into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage
                .insert_edge(id, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 2);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);
        assert!(
            results.is_empty(),
            "2 entities should not trigger 3-bidder threshold"
        );
    }

    #[test]
    fn test_cluster_score_scales_with_entity_count() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:S1".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "High Density House".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        for i in 0..6usize {
            let eid = EntityId(format!("ENT:S{}", i));
            storage.insert_node(
                eid.clone(),
                NodeWeight::Entity(EntityData {
                    name: format!("Co{}", i),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage
                .insert_edge(&eid, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 6);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(results.len(), 1);
        // 6 entities: 0.7 + (6 * 0.05).min(0.25) = 0.95
        assert!(
            results[0].risk_score >= 0.9,
            "6-entity cluster should score >= 0.9"
        );
    }

    #[test]
    fn test_cluster_excludes_pure_buyers_at_shared_gov_address() {
        // Regression: 13 public authorities keyed CF-PARTY:GB-CFS-* co-located at a
        // shared government office ("Hinton House") fired a 0.95 AddressCluster. They
        // are contracting *authorities* (IssuedBy edges), not bidders. Buyers sharing
        // a government building is not a fraud signal.
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:GOV".into());
        let contract_id = EntityId("CF:contract-1".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "Hinton House, Birchwood Park Avenue".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        storage.insert_node(contract_id.clone(), make_contract("Some Framework"));

        for (id, name) in [
            (EntityId("CF-PARTY:GB-CFS-260065".into()), "NHS England"),
            (
                EntityId("CF-PARTY:GB-CFS-260230".into()),
                "Leicester City Council",
            ),
            (
                EntityId("CF-PARTY:GB-CFS-268082".into()),
                "Northamptonshire PCC",
            ),
        ] {
            make_entity_at(&storage, &id, name, &loc_id);
            // Each issues a contract → buyer.
            storage
                .insert_edge(&id, &contract_id, make_issued_by_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 4);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);
        assert!(
            results.is_empty(),
            "buyers co-located at a government office must not cluster"
        );
    }

    #[test]
    fn test_cluster_still_fires_for_suppliers_at_one_address() {
        // The genuine signal: bidders/suppliers (AwardedTo) sharing a registered
        // address — the shell-company pattern — must still fire.
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:SHELL".into());
        let contract_id = EntityId("CF:contract-2".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "Unit 4, Shell Park".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        storage.insert_node(contract_id.clone(), make_contract("Roadworks"));

        for (id, name) in [
            (EntityId("GB-COH:11111111".into()), "Alpha Build Ltd"),
            (EntityId("GB-COH:22222222".into()), "Beta Build Ltd"),
            (EntityId("GB-COH:33333333".into()), "Gamma Build Ltd"),
        ] {
            make_entity_at(&storage, &id, name, &loc_id);
            storage
                .insert_edge(&id, &contract_id, make_awarded_to_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 4);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);
        assert_eq!(results.len(), 1, "supplier address cluster must still fire");
        assert_eq!(results[0].participants.len(), 3);
    }

    #[test]
    fn test_cluster_counts_only_suppliers_when_buyers_share_address() {
        // Mixed: 3 suppliers + 2 buyers at one address. Only the 3 suppliers count;
        // with min_bidders=3 the cluster fires on suppliers alone, buyers excluded.
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:MIX".into());
        let contract_id = EntityId("CF:contract-3".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "Mixed House".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        storage.insert_node(contract_id.clone(), make_contract("Mixed"));

        for id in [
            EntityId("GB-COH:44444444".into()),
            EntityId("GB-COH:55555555".into()),
            EntityId("GB-COH:66666666".into()),
        ] {
            make_entity_at(&storage, &id, "Supplier", &loc_id);
            storage
                .insert_edge(&id, &contract_id, make_awarded_to_edge())
                .unwrap();
        }
        for id in [
            EntityId("CF-PARTY:GB-CFS-900001".into()),
            EntityId("CF-PARTY:GB-CFS-900002".into()),
        ] {
            make_entity_at(&storage, &id, "Authority", &loc_id);
            storage
                .insert_edge(&id, &contract_id, make_issued_by_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 6);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);
        assert_eq!(results.len(), 1, "3 suppliers should cluster");
        assert_eq!(
            results[0].participants.len(),
            3,
            "only suppliers counted, 2 buyers excluded"
        );
    }

    #[test]
    fn test_cluster_suppressed_at_formation_agent_postcode() {
        // 4 companies at 71-75 Shelton Street (WC2H9JQ) — a formation agent. The LOC id
        // embeds the normalised postcode, as production does: LOC:{postcode}:{house}.
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:WC2H9JQ:71".into());
        let contract_id = EntityId("CF:c-fa".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "71-75 Shelton Street".into(),
                postcode: Some("WC2H9JQ".into()),
                address_line_1: None,
                locality: None,
            }),
        );
        storage.insert_node(contract_id.clone(), make_contract("Svc"));
        for id in [
            EntityId("GB-COH:10000001".into()),
            EntityId("GB-COH:10000002".into()),
            EntityId("GB-COH:10000003".into()),
            EntityId("GB-COH:10000004".into()),
        ] {
            make_entity_at(&storage, &id, "Co", &loc_id);
            storage
                .insert_edge(&id, &contract_id, make_awarded_to_edge())
                .unwrap();
        }
        let snapshot = bake_csr(&storage, 5);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        assert!(
            detector.check(&snapshot, &id_map).is_empty(),
            "formation-agent postcode must be suppressed"
        );
    }

    #[test]
    fn test_cluster_suppressed_at_ch_default_address() {
        // 3 unrelated Scottish companies whose street is the CH default-address
        // placeholder collapse onto one LOC node — a false cluster. Suppress by name
        // marker, which works across every registrar postcode.
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:EH79HR:0".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "SC681129 - COMPANIES HOUSE DEFAULT ADDRESS".into(),
                postcode: Some("EH79HR".into()),
                address_line_1: None,
                locality: None,
            }),
        );
        for id in [
            EntityId("GB-COH:SC653328".into()),
            EntityId("GB-COH:SC673206".into()),
            EntityId("GB-COH:SC681129".into()),
        ] {
            make_entity_at(&storage, &id, "Co", &loc_id);
        }
        let snapshot = bake_csr(&storage, 4);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        assert!(
            detector.check(&snapshot, &id_map).is_empty(),
            "Companies House default address must be suppressed"
        );
    }

    #[test]
    fn test_cluster_normal_address_not_suppressed() {
        // A genuine small address (not on any allowlist) with 3 suppliers must still fire.
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:LL652AB:12".into());
        let contract_id = EntityId("CF:c-ok".into());
        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "12 Real Street, Holyhead".into(),
                postcode: Some("LL652AB".into()),
                address_line_1: None,
                locality: None,
            }),
        );
        storage.insert_node(contract_id.clone(), make_contract("Svc"));
        for id in [
            EntityId("GB-COH:20000001".into()),
            EntityId("GB-COH:20000002".into()),
            EntityId("GB-COH:20000003".into()),
        ] {
            make_entity_at(&storage, &id, "Co", &loc_id);
            storage
                .insert_edge(&id, &contract_id, make_awarded_to_edge())
                .unwrap();
        }
        let snapshot = bake_csr(&storage, 4);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        assert_eq!(
            detector.check(&snapshot, &id_map).len(),
            1,
            "a normal address cluster must still fire"
        );
    }

    #[test]
    fn test_postcode_of_loc_id() {
        assert_eq!(postcode_of_loc_id("LOC:WC2H9JQ:71"), Some("WC2H9JQ"));
        assert_eq!(postcode_of_loc_id("LOC:EC2A4NE:86"), Some("EC2A4NE"));
        assert_eq!(postcode_of_loc_id("not-a-loc-id"), None);
    }

    #[test]
    fn test_ch_default_address_marker() {
        assert!(is_ch_default_address(
            "04719625 - COMPANIES HOUSE DEFAULT ADDRESS"
        ));
        assert!(is_ch_default_address(
            "sc681129 - companies house default address"
        ));
        assert!(!is_ch_default_address("12 Real Street, Holyhead"));
    }

    #[test]
    fn test_embedded_postcode_list_parses() {
        // The embedded data file must load the known formation-agent postcodes and
        // ignore comments/blank lines.
        let set = parse_postcode_list(FORMATION_AGENT_POSTCODES_RAW);
        assert!(set.contains("WC2H9JQ"), "Shelton Street must be present");
        assert!(set.contains("EC2A4NE"), "Paul Street must be present");
        assert!(!set.iter().any(|p| p.starts_with('#')), "no comment lines");
        assert!(!set.contains(""), "no blank entries");
    }

    #[test]
    fn test_parse_postcode_list_normalises() {
        let set = parse_postcode_list("wc2h 9jq  # lowercase w/ space\n\n# comment only\nN1 7GU\n");
        assert!(set.contains("WC2H9JQ"));
        assert!(set.contains("N17GU"));
        assert_eq!(set.len(), 2);
    }

    /// D-21/LFA-06: Contract -> LocatedAt -> Location, attributed back to the applicant via a
    /// reversed AwardedTo lookup. Three director Entities each awarded one Contract, each
    /// Contract's `LocatedAt` edge pointing at the SAME shared Location — the resulting
    /// participant list must contain the three director entity ids, never the contract ids.
    #[test]
    fn test_cluster_detects_contract_sourced_address_cluster_attributed_to_applicant() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:CONTRACT001".into());
        let d1 = EntityId("director:alpha-developments".into());
        let d2 = EntityId("director:beta-holdings".into());
        let d3 = EntityId("director:gamma-construction".into());
        let c1 = EntityId("planning:contract-1".into());
        let c2 = EntityId("planning:contract-2".into());
        let c3 = EntityId("planning:contract-3".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "1 Contract Way, Holyhead".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );

        for ((entity_id, name), contract_id) in [
            (&d1, "Alpha Developments"),
            (&d2, "Beta Holdings"),
            (&d3, "Gamma Construction"),
        ]
        .into_iter()
        .zip([&c1, &c2, &c3])
        {
            storage.insert_node(
                entity_id.clone(),
                NodeWeight::Entity(EntityData {
                    name: name.into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            storage.insert_node(contract_id.clone(), make_contract("Planning application"));
            storage
                .insert_edge(entity_id, contract_id, make_awarded_to_edge())
                .unwrap();
            storage
                .insert_edge(contract_id, &loc_id, make_located_at_edge())
                .unwrap();
        }

        let snapshot = bake_csr(&storage, 7);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(
            results.len(),
            1,
            "3 contract-sourced applicants at one address should cluster"
        );
        let mut participants = results[0].participants.clone();
        participants.sort();
        assert_eq!(
            participants,
            vec![
                "director:alpha-developments".to_string(),
                "director:beta-holdings".to_string(),
                "director:gamma-construction".to_string(),
            ],
            "participants must be applicant entity ids, never contract ids"
        );
    }

    /// Mixed old/new edge shape: two entities have a legacy entity-sourced `LocatedAt` edge,
    /// one of them ALSO has a contract-sourced `LocatedAt` edge at the same location (via its
    /// own awarded contract), and a third entity is attributed ONLY through the contract path.
    /// The combined participant list must dedup to exactly 3 distinct entities.
    #[test]
    fn test_cluster_dedups_mixed_legacy_and_contract_sourced_located_at() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:MIXEDSRC001".into());
        let d1 = EntityId("director:alpha-developments".into());
        let d2 = EntityId("director:beta-holdings".into());
        let d3 = EntityId("director:gamma-construction".into());
        let c1 = EntityId("planning:contract-mixed-1".into());
        let c3 = EntityId("planning:contract-mixed-3".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "2 Mixed Source Road, Holyhead".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );

        // d1: legacy entity-sourced LocatedAt AND a contract-sourced LocatedAt at the same
        // location (via its own awarded contract c1) — must still count once.
        make_entity_at(&storage, &d1, "Alpha Developments", &loc_id);
        storage.insert_node(c1.clone(), make_contract("App 1"));
        storage
            .insert_edge(&d1, &c1, make_awarded_to_edge())
            .unwrap();
        storage
            .insert_edge(&c1, &loc_id, make_located_at_edge())
            .unwrap();

        // d2: legacy entity-sourced LocatedAt only.
        make_entity_at(&storage, &d2, "Beta Holdings", &loc_id);

        // d3: contract-only-attributed — no direct entity-sourced LocatedAt edge of its own,
        // only reachable via its awarded contract c3's LocatedAt edge.
        storage.insert_node(
            d3.clone(),
            NodeWeight::Entity(EntityData {
                name: "Gamma Construction".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage.insert_node(c3.clone(), make_contract("App 3"));
        storage
            .insert_edge(&d3, &c3, make_awarded_to_edge())
            .unwrap();
        storage
            .insert_edge(&c3, &loc_id, make_located_at_edge())
            .unwrap();

        let snapshot = bake_csr(&storage, 6);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(results.len(), 1, "mixed-shape cluster should still fire");
        assert_eq!(
            results[0].participants.len(),
            3,
            "combined entity-sourced and contract-sourced paths must dedup to 3 distinct \
             participants, got {:?}",
            results[0].participants
        );
    }

    /// The `director:unknown-applicant` placeholder must not count as a participant via the
    /// new contract-sourced path either — mirrors
    /// `test_placeholder_applicant_is_not_counted_as_a_bidder`'s entity-path guarantee.
    #[test]
    fn test_cluster_excludes_placeholder_applicant_via_contract_sourced_path() {
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:PLACEHOLDERCONTRACT001".into());
        let d1 = EntityId("director:a-example-applicant".into());
        let d2 = EntityId("director:jordan-sample".into());
        let placeholder = EntityId("director:unknown-applicant".into());
        let c_placeholder = EntityId("planning:contract-placeholder".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "3 Placeholder Close, Llaneilian".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );

        make_entity_at(&storage, &d1, "A Example Applicant", &loc_id);
        make_entity_at(&storage, &d2, "Jordan Sample", &loc_id);

        storage.insert_node(
            placeholder.clone(),
            NodeWeight::Entity(EntityData {
                name: "Unknown Applicant".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage.insert_node(c_placeholder.clone(), make_contract("App placeholder"));
        storage
            .insert_edge(&placeholder, &c_placeholder, make_awarded_to_edge())
            .unwrap();
        storage
            .insert_edge(&c_placeholder, &loc_id, make_located_at_edge())
            .unwrap();

        let snapshot = bake_csr(&storage, 5);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 3,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);

        // Two real entity-sourced applicants plus the contract-sourced placeholder: with the
        // placeholder excluded on this path too, only 2 real parties remain — below
        // min_bidders, so it must not fire.
        assert!(
            results.is_empty(),
            "placeholder attributed via the contract-sourced path must not count as a \
             participant; got {results:?}"
        );
    }

    #[test]
    fn test_cluster_ignores_non_entity_neighbors() {
        // Entity → Location but the location has only an Officer elsewhere — no cluster from Officer
        let storage = GraphStorage::new();
        let loc_id = EntityId("LOC:O1".into());
        let off_id = EntityId("OFF:O1".into());

        storage.insert_node(
            loc_id.clone(),
            NodeWeight::Location(LocationData {
                name: "Officers Only Rd".into(),
                postcode: None,
                address_line_1: None,
                locality: None,
            }),
        );
        storage.insert_node(
            off_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "No Entity".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );
        // Officer → Location edge (not Entity → Location, so should not count)
        storage
            .insert_edge(&off_id, &loc_id, make_directs_edge())
            .unwrap();

        let snapshot = bake_csr(&storage, 1);
        let id_map = make_id_map(&storage);
        let detector = ClusterDetector::new(ClusterConfig {
            min_bidders: 1,
            ..Default::default()
        });
        let results = detector.check(&snapshot, &id_map);
        assert!(results.is_empty(), "Officers should not count as bidders");
    }
}
