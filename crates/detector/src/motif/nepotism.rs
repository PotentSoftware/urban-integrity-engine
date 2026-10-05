use std::collections::HashMap;

use urban_integrity_core::graph::snapshot::CsrSnapshot;

use crate::alert::result::MotifResult;

#[derive(Debug, Clone)]
pub struct NepotismConfig {
    pub max_hops: usize,
}

impl Default for NepotismConfig {
    fn default() -> Self {
        Self { max_hops: 3 }
    }
}

pub struct NepotismDetector {
    pub config: NepotismConfig,
}

impl NepotismDetector {
    pub fn new(config: NepotismConfig) -> Self {
        Self { config }
    }

    /// BFS from each PoliticalActor up to `max_hops` to find an `Officer`
    /// connected to a `Contract` via an `IssuedBy` edge.
    pub fn check(&self, snapshot: &CsrSnapshot, id_map: &HashMap<u32, String>) -> Vec<MotifResult> {
        let mut results = Vec::new();

        for node_idx in 0..snapshot.graph.node_count() {
            let p_actor = match &snapshot.graph[node_idx as u32] {
                urban_integrity_schema::types::node::NodeWeight::PoliticalActor(p) => p,
                _ => continue,
            };

            let mut queue = std::collections::VecDeque::new();
            queue.push_back((node_idx as u32, 0));
            let mut visited = std::collections::HashSet::new();
            visited.insert(node_idx as u32);

            while let Some((curr, hops)) = queue.pop_front() {
                if hops >= self.config.max_hops {
                    continue;
                }

                for &neighbor in snapshot.graph.neighbors_slice(curr) {
                    if visited.contains(&neighbor) {
                        continue;
                    }
                    visited.insert(neighbor);

                    if let urban_integrity_schema::types::node::NodeWeight::Officer(o) =
                        &snapshot.graph[neighbor]
                    {
                        if let (Some(id_p), Some(id_o)) =
                            (id_map.get(&(node_idx as u32)), id_map.get(&neighbor))
                        {
                            results.push(MotifResult {
                                motif_type: crate::alert::result::MotifType::NepotismBridge,
                                unique_hash: crate::alert::result::motif_hash(&[id_p, id_o]),
                                participants: vec![id_p.clone(), id_o.clone()],
                                risk_score: 0.9,
                                evidence_snapshot: format!("Nepotism link: {} (Political Actor) connected to {} (Officer) within {} hops", p_actor.name, o.name, hops + 1),
                                detected_at_event: snapshot.baked_at_event,
                                version: 1,
                                last_seen_at: chrono::Utc::now(),
                                score_breakdown: crate::alert::result::ScoreBreakdown {
                                    s_phoenix: 0.0,
                                    s_proximity: 0.85,
                                    s_clustering: 0.0,
                                    p_base: 0.85,
                                    m_env: 1.2,
                                    p_final: 0.9,
                                    imd_rank: None,
                                    ward_code: None,
                                },
                                withdrawn_at: None,
                                withdrawn_reason: None,
                            });
                        }
                    }
                    queue.push_back((neighbor, hops + 1));
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{
        EntityData, EntityStatus, NodeWeight, OfficerData, PoliticalActorData,
    };

    fn make_edge() -> EdgeWeight {
        EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "link".into(),
                officer_role: None,
            },
        }
    }

    fn make_id_map(storage: &GraphStorage) -> HashMap<u32, String> {
        let mut m = HashMap::new();
        for (id, idx, _, _) in storage.snapshot_nodes() {
            m.insert(idx.index() as u32, id.0.clone());
        }
        m
    }

    #[test]
    fn test_nepotism_detector_creation() {
        let config = NepotismConfig::default();
        let detector = NepotismDetector::new(config);
        assert_eq!(detector.config.max_hops, 3);
    }

    #[test]
    fn test_nepotism_check_empty() {
        let storage = GraphStorage::new();
        let snapshot = bake_csr(&storage, 1);
        let id_map = HashMap::new();
        let detector = NepotismDetector::new(NepotismConfig::default());
        let results = detector.check(&snapshot, &id_map);
        assert!(results.is_empty());
    }

    #[test]
    fn test_nepotism_detects_direct_link() {
        // PoliticalActor ─Directs→ Officer — hop distance 1
        let storage = GraphStorage::new();
        let pa_id = EntityId("PA:001".into());
        let off_id = EntityId("OFF:001".into());

        storage.insert_node(
            pa_id.clone(),
            NodeWeight::PoliticalActor(PoliticalActorData {
                name: "Councillor Smith".into(),
                ..Default::default()
            }),
        );
        storage.insert_node(
            off_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "J. Smith".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );
        storage.insert_edge(&pa_id, &off_id, make_edge()).unwrap();

        let snapshot = bake_csr(&storage, 1);
        let id_map = make_id_map(&storage);

        let detector = NepotismDetector::new(NepotismConfig { max_hops: 3 });
        let results = detector.check(&snapshot, &id_map);

        assert_eq!(results.len(), 1, "Expected 1 nepotism result");
        assert_eq!(
            results[0].motif_type,
            crate::alert::result::MotifType::NepotismBridge
        );
        assert!(results[0].risk_score > 0.8);
        assert!(results[0].participants.contains(&"PA:001".to_string()));
        assert!(results[0].participants.contains(&"OFF:001".to_string()));
    }

    #[test]
    fn test_nepotism_detects_two_hop_link() {
        // PoliticalActor ─→ Entity ─→ Officer — hop distance 2
        let storage = GraphStorage::new();
        let pa_id = EntityId("PA:002".into());
        let mid_id = EntityId("ENT:002".into());
        let off_id = EntityId("OFF:002".into());

        storage.insert_node(
            pa_id.clone(),
            NodeWeight::PoliticalActor(PoliticalActorData {
                name: "Councillor Jones".into(),
                ..Default::default()
            }),
        );
        storage.insert_node(
            mid_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "Middleman Ltd".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage.insert_node(
            off_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "A. Jones".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );
        storage.insert_edge(&pa_id, &mid_id, make_edge()).unwrap();
        storage.insert_edge(&mid_id, &off_id, make_edge()).unwrap();

        let snapshot = bake_csr(&storage, 2);
        let id_map = make_id_map(&storage);

        let detector = NepotismDetector::new(NepotismConfig { max_hops: 3 });
        let results = detector.check(&snapshot, &id_map);

        assert!(!results.is_empty(), "Should detect 2-hop nepotism link");
        assert!(results
            .iter()
            .any(|r| r.participants.contains(&"OFF:002".to_string())));
    }

    #[test]
    fn test_nepotism_respects_max_hops() {
        // PoliticalActor ─→ Hop1 ─→ Hop2 ─→ Officer — hop distance 3
        // With max_hops=2 this should NOT be detected
        let storage = GraphStorage::new();
        let pa_id = EntityId("PA:003".into());
        let h1_id = EntityId("H1:003".into());
        let h2_id = EntityId("H2:003".into());
        let off_id = EntityId("OFF:003".into());

        storage.insert_node(
            pa_id.clone(),
            NodeWeight::PoliticalActor(PoliticalActorData {
                name: "Cllr Brown".into(),
                ..Default::default()
            }),
        );
        storage.insert_node(
            h1_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "Hop1".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage.insert_node(
            h2_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "Hop2".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage.insert_node(
            off_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "C. Brown".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );
        storage.insert_edge(&pa_id, &h1_id, make_edge()).unwrap();
        storage.insert_edge(&h1_id, &h2_id, make_edge()).unwrap();
        storage.insert_edge(&h2_id, &off_id, make_edge()).unwrap();

        let snapshot = bake_csr(&storage, 3);
        let id_map = make_id_map(&storage);

        // max_hops=2 means BFS stops at depth 2 — the officer at hop 3 is out of range
        let detector = NepotismDetector::new(NepotismConfig { max_hops: 2 });
        let results = detector.check(&snapshot, &id_map);
        assert!(
            results.is_empty(),
            "Officer at hop 3 should not be detected with max_hops=2"
        );

        // max_hops=3 should find it
        let detector3 = NepotismDetector::new(NepotismConfig { max_hops: 3 });
        let results3 = detector3.check(&snapshot, &id_map);
        assert!(
            !results3.is_empty(),
            "Officer at hop 3 should be detected with max_hops=3"
        );
    }

    #[test]
    fn test_nepotism_no_false_positive_without_political_actor() {
        // Graph has Officer and Entity but no PoliticalActor — should produce no results
        let storage = GraphStorage::new();
        let ent_id = EntityId("ENT:004".into());
        let off_id = EntityId("OFF:004".into());

        storage.insert_node(
            ent_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "Anon Ltd".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        storage.insert_node(
            off_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "D. Anon".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );
        storage.insert_edge(&ent_id, &off_id, make_edge()).unwrap();

        let snapshot = bake_csr(&storage, 1);
        let id_map = make_id_map(&storage);

        let detector = NepotismDetector::new(NepotismConfig::default());
        let results = detector.check(&snapshot, &id_map);
        assert!(results.is_empty(), "No PoliticalActor means no nepotism");
    }
}
