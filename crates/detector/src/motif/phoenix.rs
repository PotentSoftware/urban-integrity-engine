use std::collections::HashMap;

use chrono::{DateTime, Utc};
use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_schema::types::node::{EntityStatus, NodeWeight};

use crate::alert::result::{motif_hash, MotifResult, MotifType, ScoreBreakdown};

#[derive(Debug, Clone)]
pub struct PhoenixConfig {
    pub detection_window_days: i64,
    pub min_value_threshold: f64,
}

impl Default for PhoenixConfig {
    fn default() -> Self {
        Self {
            detection_window_days: 180,
            min_value_threshold: 10000.0,
        }
    }
}

pub struct PhoenixDetector {
    pub config: PhoenixConfig,
}

impl PhoenixDetector {
    pub fn new(config: PhoenixConfig) -> Self {
        Self { config }
    }

    /// Tier A: Real-time hot-path check on a single newly ingested entity.
    /// Returns empty vec — detailed detection runs in Tier B on the CSR snapshot.
    pub fn check_tier_a(
        &self,
        _storage: &urban_integrity_core::graph::storage::GraphStorage,
        _id: &urban_integrity_schema::types::id::EntityId,
    ) -> Vec<MotifResult> {
        Vec::new()
    }

    /// Tier B: Delta-based CSR scan with 6-month date window enforcement (MOTF-01).
    ///
    /// For each seed (active company):
    /// 1. Find officers connected to it
    /// 2. For each officer, find dissolved/in-liquidation entities within the 180-day window
    ///    relative to the active company's earliest contract award date
    /// 3. Group by officer; apply stochastic multiplier for serial failures
    /// 4. Emit one MotifResult per (active, failed) pair with adjusted score
    pub fn check_tier_b(
        &self,
        snapshot: &CsrSnapshot,
        delta_nodes: &[u32],
        id_map: &HashMap<u32, String>,
    ) -> Vec<MotifResult> {
        let mut results = Vec::new();

        for &seed_idx in delta_nodes {
            // Only consider active companies as the seed
            let seed_entity = match &snapshot.graph[seed_idx] {
                NodeWeight::Entity(e) if matches!(e.status, EntityStatus::Active) => e,
                _ => continue,
            };

            // Find the earliest award date for contracts connected to this active company.
            // AwardedTo edge direction: Contract is a neighbor of active company (or vice versa).
            // We look at all neighbors of seed_idx for ContractData nodes.
            let award_date: Option<DateTime<Utc>> = snapshot
                .graph
                .neighbors_slice(seed_idx)
                .iter()
                .filter_map(|&n_idx| {
                    if let NodeWeight::Contract(c) = &snapshot.graph[n_idx] {
                        c.date_published.as_deref().and_then(|s| {
                            // Try RFC3339 first, then plain date
                            DateTime::parse_from_rfc3339(s)
                                .map(|dt| dt.with_timezone(&Utc))
                                .or_else(|_| {
                                    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
                                })
                                .ok()
                        })
                    } else {
                        None
                    }
                })
                .min(); // earliest award date

            // If no contract with a parseable date, skip (conservative — no false positives)
            let award_date = match award_date {
                Some(d) => d,
                None => continue,
            };

            // For each officer connected to the seed, collect qualifying dissolved entities
            // officer_idx -> Vec<(dissolved_entity_idx, dissolved_dt, is_liquidation)>
            let mut officer_map: HashMap<u32, Vec<(u32, DateTime<Utc>, bool)>> = HashMap::new();

            // Both hops traverse Directs edges UNDIRECTED (planning open item 20).
            //
            // The motif is seed -> officer -> other_company, so the two hops run in opposite
            // directions over the same relation. A directed, forward-only CSR cannot express
            // that: whichever way Directs points, one hop always has no edge to follow. This
            // is why Phoenix had never fired, and why simply inverting the ingest direction
            // does not help — it only moves the failure from hop 2 to hop 1 (verified live:
            // a bake after inverting 22,807 edges still produced zero alerts).
            //
            // Direction-agnostic traversal is also the only shape robust to the two ingest
            // paths disagreeing — ch_backfill.py writes company -> officer, ch/mapper.rs
            // writes officer -> company, and both appear in the live graph.
            for officer_idx in snapshot.neighbors_undirected(seed_idx) {
                let officer = match &snapshot.graph[officer_idx] {
                    NodeWeight::Officer(o) => o,
                    _ => continue,
                };

                // A corporate officer is a service provider, not a phoenix director.
                // Companies-secretarial firms (Example Corporate Secretary Limited and
                // the like) are the named officer of many unrelated companies, so a motif
                // through one joins companies that share nothing but a supplier. This is
                // the same hub-node false positive `director:unknown-applicant` produced in
                // AddressCluster (Open Item 6b), and it is T-23 (false accusation) when it
                // reaches an alert. CH marks them all with a `corporate-` role prefix.
                if officer
                    .officer_role
                    .as_deref()
                    .is_some_and(|r| r.starts_with("corporate-"))
                {
                    continue;
                }

                for other_idx in snapshot.neighbors_undirected(officer_idx) {
                    if other_idx == seed_idx {
                        continue; // skip the seed itself
                    }
                    let (dissolved_dt, is_liquidation) = match &snapshot.graph[other_idx] {
                        NodeWeight::Entity(e) => match &e.status {
                            EntityStatus::Dissolved(dt) => (*dt, false),
                            EntityStatus::InLiquidation(dt) => (*dt, true),
                            EntityStatus::Active => continue,
                        },
                        _ => continue,
                    };

                    // 6-month (180-day) window check
                    let days_diff = (award_date - dissolved_dt).num_days().abs();
                    if days_diff <= self.config.detection_window_days {
                        officer_map.entry(officer_idx).or_default().push((
                            other_idx,
                            dissolved_dt,
                            is_liquidation,
                        ));
                    }
                }
            }

            // Emit results grouped by officer with stochastic multiplier
            for (officer_idx, failures) in &officer_map {
                let n = failures.len() as i32;
                // P_final = 1.0 - (0.15)^n, clamped to [0, 1]
                let p_final = (1.0_f64 - 0.15_f64.powi(n)).min(1.0_f64);

                for (dissolved_idx, _dissolved_dt, is_liquidation) in failures {
                    if let (Some(id_a), Some(id_b)) =
                        (id_map.get(&seed_idx), id_map.get(dissolved_idx))
                    {
                        let motif_subtype = if *is_liquidation {
                            "pre_emptive_phoenix"
                        } else {
                            "hard_phoenix"
                        };
                        let officer_id = id_map.get(officer_idx).cloned().unwrap_or_default();

                        results.push(MotifResult {
                            motif_type: MotifType::PhoenixLoop,
                            unique_hash: motif_hash(&[id_a, id_b]),
                            participants: vec![id_a.clone(), id_b.clone()],
                            risk_score: p_final,
                            evidence_snapshot: format!(
                                "Phoenix pattern [{motif_subtype}]: {} shares officer {} \
                                 with failed entity {}; {} serial failure(s) in window",
                                seed_entity.name, officer_id, id_b, n
                            ),
                            detected_at_event: snapshot.baked_at_event,
                            version: 1,
                            last_seen_at: chrono::Utc::now(),
                            score_breakdown: ScoreBreakdown {
                                s_phoenix: p_final,
                                s_proximity: 0.0,
                                s_clustering: 0.0,
                                p_base: 0.85,
                                m_env: 1.0,
                                p_final,
                                imd_rank: None,
                                ward_code: None,
                            },
                            withdrawn_at: None,
                            withdrawn_reason: None,
                        });
                    }
                }
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{AwardedToData, DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{
        ContractData, EntityData, EntityStatus, NodeWeight, OfficerData,
    };

    fn make_directs_edge() -> EdgeWeight {
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

    fn make_awarded_to_edge() -> EdgeWeight {
        EdgeWeight::AwardedTo {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: AwardedToData {
                details: "award".into(),
            },
        }
    }

    fn build_id_map(storage: &GraphStorage) -> HashMap<u32, String> {
        let mut m = HashMap::new();
        for (id, idx, _, _) in storage.snapshot_nodes() {
            m.insert(idx.index() as u32, id.0.clone());
        }
        m
    }

    #[test]
    fn test_phoenix_detector_creation() {
        let config = PhoenixConfig::default();
        let detector = PhoenixDetector::new(config);
        assert_eq!(detector.config.detection_window_days, 180);
    }

    #[test]
    fn test_phoenix_check_tier_a() {
        let storage = GraphStorage::new();
        storage.insert_node(
            EntityId("GB-COH:123".to_string()),
            NodeWeight::Entity(EntityData {
                name: "A".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        let detector = PhoenixDetector::new(PhoenixConfig::default());
        let results = detector.check_tier_a(&storage, &EntityId("GB-COH:123".to_string()));
        assert!(results.is_empty());
    }

    #[test]
    fn test_phoenix_check_tier_b() {
        // Single active company with no contract — no award_date → no result
        let storage = GraphStorage::new();
        storage.insert_node(
            EntityId("GB-COH:123".to_string()),
            NodeWeight::Entity(EntityData {
                name: "A".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        let snapshot = bake_csr(&storage, 1);
        let id_map = build_id_map(&storage);

        let detector = PhoenixDetector::new(PhoenixConfig::default());
        let results = detector.check_tier_b(&snapshot, &[0], &id_map);
        assert!(results.is_empty());
    }

    /// MOTF-01 test 1: dissolution 90 days before contract award — within 180-day window → fires
    /// A dissolution 190 days from the award must NOT fire against a 180-day window.
    ///
    /// Mirrors a live pair from the 2026-08-11 bake (identifiers here are synthetic): an
    /// active seed company holds one contract published 2026-07-01, and shares a director
    /// with a second company dissolved 2025-12-23. That is 190 days — outside the configured
    /// window — yet an alert was emitted. This pins the boundary with the same dates.
    #[test]
    fn test_phoenix_window_outside_does_not_fire() {
        let award_date = chrono::DateTime::parse_from_rfc3339("2026-07-01T08:51:18+01:00")
            .unwrap()
            .with_timezone(&Utc);
        let dissolution_date = chrono::DateTime::parse_from_rfc3339("2025-12-23T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            (award_date - dissolution_date).num_days(),
            190,
            "fixture must reproduce the live 190-day gap"
        );

        let storage = GraphStorage::new();
        let active_id = EntityId("GB-COH:99000021".to_string());
        storage.insert_node(
            active_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "EXAMPLE CARE (NORTH) LIMITED".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
        let officer_id = EntityId("director:sample-alex-b197001".to_string());
        storage.insert_node(
            officer_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "SAMPLE, Alex".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );
        let dissolved_id = EntityId("GB-COH:99000022".to_string());
        storage.insert_node(
            dissolved_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "SAMPLE HEALTHCARE (SOUTH) LIMITED".into(),
                company_number: None,
                status: EntityStatus::Dissolved(dissolution_date),
                incorporation_date: None,
            }),
        );
        let contract_id = EntityId("CF:live".to_string());
        storage.insert_node(
            contract_id.clone(),
            NodeWeight::Contract(ContractData {
                name: "Example Residential and Nursing Care Homes Provider List".into(),
                ocid: None,
                value_gbp: None,
                status: None,
                date_published: Some(award_date.to_rfc3339()),
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            }),
        );
        storage
            .insert_edge(&active_id, &officer_id, make_directs_edge())
            .unwrap();
        storage
            .insert_edge(&dissolved_id, &officer_id, make_directs_edge())
            .unwrap();
        storage
            .insert_edge(&active_id, &contract_id, make_awarded_to_edge())
            .unwrap();

        let snapshot = bake_csr(&storage, 4);
        let id_map = build_id_map(&storage);
        let active_csr_idx = id_map
            .iter()
            .find(|(_, v)| *v == "GB-COH:99000021")
            .map(|(k, _)| *k)
            .expect("active company not in id_map");

        let detector = PhoenixDetector::new(PhoenixConfig::default());
        let results = detector.check_tier_b(&snapshot, &[active_csr_idx], &id_map);

        assert!(
            results.is_empty(),
            "190 days is outside the 180-day window — got {} alert(s): {:?}",
            results.len(),
            results
                .iter()
                .map(|r| &r.evidence_snapshot)
                .collect::<Vec<_>>()
        );
    }

    /// A corporate officer is a service provider, not a phoenix director.
    ///
    /// Found live on 2026-08-15: after backfilling officers for 194 CF awardee companies,
    /// Phoenix's first and only would-be motif ran through
    /// `director:example-corporate-secretary-limited` — a company-secretarial firm that is
    /// the named secretary of many unrelated companies. Like `director:unknown-applicant`
    /// in AddressCluster (Open Item 6b), such a node joins companies that share nothing but
    /// a supplier, and every motif through it is a false accusation (T-23).
    ///
    /// CH marks these with a `corporate-` role prefix: corporate-secretary,
    /// corporate-director, corporate-llp-member, corporate-llp-designated-member,
    /// corporate-nominee-secretary. 290 of the graph's 21,045 officers are corporate.
    #[test]
    fn corporate_officers_do_not_carry_a_phoenix_motif() {
        let award_date = Utc::now();
        let dissolution_date = award_date - Duration::days(90); // comfortably inside the window

        for role in [
            "corporate-secretary",
            "corporate-director",
            "corporate-llp-member",
            "corporate-llp-designated-member",
            "corporate-nominee-secretary",
        ] {
            let storage = GraphStorage::new();
            let active_id = EntityId("GB-COH:99000023".to_string());
            storage.insert_node(
                active_id.clone(),
                NodeWeight::Entity(EntityData {
                    name: "ActiveCo".into(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            );
            let officer_id = EntityId("director:example-corporate-secretary-limited".to_string());
            storage.insert_node(
                officer_id.clone(),
                NodeWeight::Officer(OfficerData {
                    name: "EXAMPLE CORPORATE SECRETARY LIMITED".into(),
                    officer_role: Some(role.to_owned()),
                    appointed_on: None,
                    resigned_on: None,
                    company_number: None,
                }),
            );
            let dissolved_id = EntityId("GB-COH:99000024".to_string());
            storage.insert_node(
                dissolved_id.clone(),
                NodeWeight::Entity(EntityData {
                    name: "FailedCo".into(),
                    company_number: None,
                    status: EntityStatus::Dissolved(dissolution_date),
                    incorporation_date: None,
                }),
            );
            let contract_id = EntityId("CF:ocds-live".to_string());
            storage.insert_node(
                contract_id.clone(),
                NodeWeight::Contract(ContractData {
                    name: "A contract".into(),
                    ocid: None,
                    value_gbp: None,
                    status: None,
                    date_published: Some(award_date.to_rfc3339()),
                    ward_code: None,
                    tender_ref: None,
                    application_ref: None,
                    application_received_date: None,
                    applicant_provenance: None,
                    portal_detail_handle: None,
                }),
            );
            storage
                .insert_edge(&active_id, &officer_id, make_directs_edge())
                .unwrap();
            storage
                .insert_edge(&dissolved_id, &officer_id, make_directs_edge())
                .unwrap();
            storage
                .insert_edge(&active_id, &contract_id, make_awarded_to_edge())
                .unwrap();

            let snapshot = bake_csr(&storage, 4);
            let id_map = build_id_map(&storage);
            let seed = id_map
                .iter()
                .find(|(_, v)| *v == "GB-COH:99000023")
                .map(|(k, _)| *k)
                .expect("seed not in id_map");

            let detector = PhoenixDetector::new(PhoenixConfig::default());
            let results = detector.check_tier_b(&snapshot, &[seed], &id_map);

            assert!(
                results.is_empty(),
                "role {role:?} is a corporate officer and must not carry a motif — got {:?}",
                results
                    .iter()
                    .map(|r| &r.evidence_snapshot)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_phoenix_window_within_fires() {
        let award_date = Utc::now();
        let dissolution_date = award_date - Duration::days(90); // within 180-day window

        let storage = GraphStorage::new();

        // Active company (seed)
        let active_id = EntityId("GB-COH:active".to_string());
        storage.insert_node(
            active_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "ActiveCo".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );

        // Officer
        let officer_id = EntityId("officer-1".to_string());
        storage.insert_node(
            officer_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "J Smith".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );

        // Dissolved company
        let dissolved_id = EntityId("GB-COH:dissolved".to_string());
        storage.insert_node(
            dissolved_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "OldCo".into(),
                company_number: None,
                status: EntityStatus::Dissolved(dissolution_date),
                incorporation_date: None,
            }),
        );

        // Contract node with award date
        let contract_id = EntityId("contract-1".to_string());
        storage.insert_node(
            contract_id.clone(),
            NodeWeight::Contract(ContractData {
                name: "Test Contract".into(),
                ocid: None,
                value_gbp: None,
                status: None,
                date_published: Some(award_date.to_rfc3339()),
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            }),
        );

        // Directs edges BOTH point company -> officer, which is what `ch_backfill.py`
        // actually writes (22,807 live edges). This fixture previously inserted the second
        // hop as officer -> dissolved, a shape NO ingest path produces — so the test passed
        // against a graph that cannot exist, and Phoenix's inability to fire on real data
        // went unnoticed from the day it was written (planning open item 20).
        storage
            .insert_edge(&active_id, &officer_id, make_directs_edge())
            .unwrap();
        storage
            .insert_edge(&dissolved_id, &officer_id, make_directs_edge())
            .unwrap();
        storage
            .insert_edge(&active_id, &contract_id, make_awarded_to_edge())
            .unwrap();

        let snapshot = bake_csr(&storage, 4);
        let id_map = build_id_map(&storage);

        // Find the CSR index of the active company node
        let active_csr_idx = id_map
            .iter()
            .find(|(_, v)| *v == "GB-COH:active")
            .map(|(k, _)| *k)
            .expect("active company not in id_map");

        let detector = PhoenixDetector::new(PhoenixConfig::default());
        let results = detector.check_tier_b(&snapshot, &[active_csr_idx], &id_map);

        assert_eq!(
            results.len(),
            1,
            "Expected 1 Phoenix result within 180-day window (dissolution 90 days before award)"
        );
        assert_eq!(results[0].motif_type, MotifType::PhoenixLoop);
    }

    /// MOTF-01 test 2: dissolution 400 days before contract award — outside 180-day window → silent
    #[test]
    fn test_phoenix_window_outside_silent() {
        let award_date = Utc::now();
        let dissolution_date = award_date - Duration::days(400); // outside 180-day window

        let storage = GraphStorage::new();

        let active_id = EntityId("GB-COH:active2".to_string());
        storage.insert_node(
            active_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "ActiveCo2".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );

        let officer_id = EntityId("officer-2".to_string());
        storage.insert_node(
            officer_id.clone(),
            NodeWeight::Officer(OfficerData {
                name: "A Jones".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        );

        let dissolved_id = EntityId("GB-COH:dissolved2".to_string());
        storage.insert_node(
            dissolved_id.clone(),
            NodeWeight::Entity(EntityData {
                name: "OldCo2".into(),
                company_number: None,
                status: EntityStatus::Dissolved(dissolution_date),
                incorporation_date: None,
            }),
        );

        let contract_id = EntityId("contract-2".to_string());
        storage.insert_node(
            contract_id.clone(),
            NodeWeight::Contract(ContractData {
                name: "Test Contract 2".into(),
                ocid: None,
                value_gbp: None,
                status: None,
                date_published: Some(award_date.to_rfc3339()),
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            }),
        );

        storage
            .insert_edge(&active_id, &officer_id, make_directs_edge())
            .unwrap();
        storage
            .insert_edge(&officer_id, &dissolved_id, make_directs_edge())
            .unwrap();
        storage
            .insert_edge(&active_id, &contract_id, make_awarded_to_edge())
            .unwrap();

        let snapshot = bake_csr(&storage, 4);
        let id_map = build_id_map(&storage);

        let active_csr_idx = id_map
            .iter()
            .find(|(_, v)| *v == "GB-COH:active2")
            .map(|(k, _)| *k)
            .expect("active2 not in id_map");

        let detector = PhoenixDetector::new(PhoenixConfig::default());
        let results = detector.check_tier_b(&snapshot, &[active_csr_idx], &id_map);

        assert!(
            results.is_empty(),
            "Expected no Phoenix result — dissolution 400 days before award is outside 180-day window"
        );
    }
}
