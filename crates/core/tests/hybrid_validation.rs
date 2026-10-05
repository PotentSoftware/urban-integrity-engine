use std::sync::Arc;
use std::time::{Duration, Instant};
use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
use urban_integrity_core::graph::types::{GraphEvent, GraphEventKind, GraphMutation};
use urban_integrity_detector::DetectorEngine;
use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{
    ContractData, EntityData, EntityStatus, NodeWeight, OfficerData,
};

#[tokio::test]
async fn test_logic_pressure_hybrid() {
    // 1. Setup Engine with real detector
    let (wal_tx, wal_rx) = crossbeam_channel::bounded::<GraphEvent>(10_000);
    let detector = Arc::new(DetectorEngine::new(Default::default()));
    let config = CoordinatorConfig {
        snapshot_every_n_events: 50,
        snapshot_interval_secs: 1,
    };

    let (coordinator, handle) = GraphCoordinator::new(config, wal_tx, Some(detector));
    tokio::spawn(coordinator.run());

    // 2. Inject "Golden Record" (Phoenix Loop)
    // Officer -> Entity (Dissolved)
    // Officer -> Entity (Active)
    let officer_id = EntityId("OFF:123".to_string());
    let active_id = EntityId("ENT:ACTIVE".to_string());
    let dissolved_id = EntityId("ENT:DISSOLVED".to_string());

    handle
        .mutation_tx
        .send(GraphMutation::AddNode {
            id: officer_id.clone(),
            weight: NodeWeight::Officer(OfficerData {
                name: "John Doe".into(),
                officer_role: None,
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        })
        .await
        .unwrap();

    handle
        .mutation_tx
        .send(GraphMutation::AddNode {
            id: active_id.clone(),
            weight: NodeWeight::Entity(EntityData {
                name: "Active Corp".into(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        })
        .await
        .unwrap();

    handle
        .mutation_tx
        .send(GraphMutation::AddNode {
            id: dissolved_id.clone(),
            weight: NodeWeight::Entity(EntityData {
                name: "Ghost Corp".into(),
                company_number: None,
                status: EntityStatus::Dissolved(chrono::Utc::now()),
                incorporation_date: None,
            }),
        })
        .await
        .unwrap();

    let edge_weight = EdgeWeight::Directs {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DirectsData {
            details: "director".into(),
            officer_role: None,
        },
    };

    // Pattern: Active -> Officer -> Dissolved (based on my forward scan implementation)
    handle
        .mutation_tx
        .send(GraphMutation::AddEdge {
            src: active_id.clone(),
            dst: officer_id.clone(),
            weight: edge_weight.clone(),
        })
        .await
        .unwrap();
    handle
        .mutation_tx
        .send(GraphMutation::AddEdge {
            src: officer_id.clone(),
            dst: dissolved_id.clone(),
            weight: edge_weight.clone(),
        })
        .await
        .unwrap();

    // Add a Contract node connected to the active entity so Phoenix detector
    // has an award_date anchor (required since Phase 8 window enforcement).
    // date_published = today → 0 days from dissolution → within 180-day window → FIRES.
    let contract_id = EntityId("CON:PHOENIX_TEST".to_string());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    handle
        .mutation_tx
        .send(GraphMutation::AddNode {
            id: contract_id.clone(),
            weight: NodeWeight::Contract(ContractData {
                name: "Test Contract".into(),
                ocid: None,
                value_gbp: Some(50_000.0), // above 10k min threshold
                status: Some("active".into()),
                date_published: Some(today),
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            }),
        })
        .await
        .unwrap();
    // Active entity Awards contract (neighbor relationship for Phoenix scan)
    let awards_weight = EdgeWeight::Directs {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DirectsData {
            details: "awards".into(),
            officer_role: None,
        },
    };
    handle
        .mutation_tx
        .send(GraphMutation::AddEdge {
            src: active_id.clone(),
            dst: contract_id.clone(),
            weight: awards_weight,
        })
        .await
        .unwrap();

    // 3. Pump "Chaos" events (synthetic noise)
    let chaos_handle = tokio::spawn(async move {
        for i in 0..500 {
            let id = EntityId(format!("noise-{}", i));
            let weight = NodeWeight::Entity(EntityData {
                name: format!("Noise {}", i),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            });
            let _ = handle
                .mutation_tx
                .send(GraphMutation::AddNode { id, weight })
                .await;
            if i % 10 == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    });

    // 4. Verify detection amidst noise
    let mut found_phoenix = false;
    let start = Instant::now();

    while start.elapsed() < Duration::from_secs(10) {
        if let Ok(event) = tokio::task::spawn_blocking({
            let rx = wal_rx.clone();
            move || rx.recv_timeout(Duration::from_millis(100))
        })
        .await
        .unwrap()
        {
            if let GraphEventKind::MotifDetected { result_json } = event.mutation {
                if result_json.contains("Phoenix pattern") {
                    found_phoenix = true;
                    break;
                }
            }
        }
    }

    chaos_handle.await.unwrap();
    assert!(
        found_phoenix,
        "Golden Record (Phoenix) must be detected despite chaos pressure"
    );
}
