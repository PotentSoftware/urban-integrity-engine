//! End-to-end regression test for the `NodeUpsert` status-transition fix.
//!
//! Before the fix, `EventPayload::NodeUpsert` was translated to the insert-only
//! `GraphMutation::AddNode`, so a company observed while `Active` that later
//! dissolved never had its status updated — the dissolution event was silently
//! dropped. That made it impossible for the Phoenix detector (which requires a
//! *dissolved* predecessor) to fire for the normal monitoring case.
//!
//! This test drives the real pipeline end-to-end: it ingests a predecessor while
//! `Active`, then re-ingests it as `Dissolved` via `UpsertNode` (the transition
//! the bug dropped), then wires up an active successor that shares an officer and
//! holds a dated contract. The Phoenix loop can only be detected if the
//! transition was actually applied to the existing node. Pre-fix, the predecessor
//! would still be `Active`, the officer→predecessor hop would hit an `Active`
//! company, and no `PhoenixLoop` would ever be emitted — so this test fails
//! without the fix and passes with it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
use urban_integrity_core::graph::types::{GraphEvent, GraphEventKind, GraphMutation};
use urban_integrity_detector::{DetectorConfig, DetectorEngine};
use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{
    ContractData, EntityData, EntityStatus, NodeWeight, OfficerData,
};

fn directs_edge(details: &str) -> EdgeWeight {
    EdgeWeight::Directs {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DirectsData {
            details: details.into(),
            officer_role: None,
        },
    }
}

fn entity(name: &str, status: EntityStatus) -> NodeWeight {
    NodeWeight::Entity(EntityData {
        name: name.into(),
        company_number: Some("12345678".into()),
        status,
        incorporation_date: None,
    })
}

#[tokio::test]
async fn test_phoenix_fires_only_after_dissolution_upsert() {
    // 1. Engine with the real Phoenix detector + a tiny IMD index so enrichment
    //    also applies deprivation weighting (E05001260 → rank 1 of max 100 → m_env≈1.99).
    let imd_path = std::env::temp_dir().join(format!("imd_{}.csv", uuid::Uuid::new_v4()));
    std::fs::write(
        &imd_path,
        "Ward Code,IMD Rank\nE05001260,1\nE05099999,100\n",
    )
    .unwrap();

    let (wal_tx, wal_rx) = crossbeam_channel::bounded::<GraphEvent>(10_000);
    let detector = Arc::new(DetectorEngine::new(DetectorConfig {
        phoenix_window_days: 730,
        phoenix_min_value: 10_000.0,
        imd_csv_path: Some(imd_path.to_string_lossy().into_owned()),
    }));
    let config = CoordinatorConfig {
        snapshot_every_n_events: 25,
        snapshot_interval_secs: 1,
    };
    let (coordinator, handle) = GraphCoordinator::new(config, wal_tx, Some(detector));
    tokio::spawn(coordinator.run());

    let predecessor_id = EntityId("GB-COH:PREDECESSOR".to_string());
    let successor_id = EntityId("GB-COH:SUCCESSOR".to_string());
    let officer_id = EntityId("OFF:SHARED-DIRECTOR".to_string());
    let contract_id = EntityId("CON:AWARD".to_string());

    // 2. First sighting of the predecessor: ACTIVE (via UpsertNode → insert).
    handle
        .mutation_tx
        .send(GraphMutation::UpsertNode {
            id: predecessor_id.clone(),
            weight: entity("Ghost Corp", EntityStatus::Active),
        })
        .await
        .unwrap();

    // 3. THE TRANSITION: re-ingest the predecessor as DISSOLVED. Pre-fix this was
    //    dropped by the insert-only path; the node would stay Active forever.
    handle
        .mutation_tx
        .send(GraphMutation::UpsertNode {
            id: predecessor_id.clone(),
            weight: entity("Ghost Corp", EntityStatus::Dissolved(chrono::Utc::now())),
        })
        .await
        .unwrap();

    // 4. Shared officer + active successor + dated contract forming the loop.
    handle
        .mutation_tx
        .send(GraphMutation::UpsertNode {
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

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    handle
        .mutation_tx
        .send(GraphMutation::UpsertNode {
            id: contract_id.clone(),
            weight: NodeWeight::Contract(ContractData {
                name: "Rebuild Contract".into(),
                ocid: None,
                value_gbp: Some(50_000.0),
                status: Some("active".into()),
                date_published: Some(today),
                ward_code: Some("E05001260".into()),
                tender_ref: None,
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            }),
        })
        .await
        .unwrap();

    // Add the active successor LAST so it is in the snapshot delta (the detector
    // seeds from Active entities in the delta).
    handle
        .mutation_tx
        .send(GraphMutation::UpsertNode {
            id: successor_id.clone(),
            weight: entity("Phoenix Corp", EntityStatus::Active),
        })
        .await
        .unwrap();

    // Edges: successor → officer → predecessor, successor → contract.
    for (src, dst, details) in [
        (&successor_id, &officer_id, "director"),
        (&officer_id, &predecessor_id, "director"),
        (&successor_id, &contract_id, "awards"),
    ] {
        handle
            .mutation_tx
            .send(GraphMutation::AddEdge {
                src: src.clone(),
                dst: dst.clone(),
                weight: directs_edge(details),
            })
            .await
            .unwrap();
    }

    // 5. Nudge the coordinator past the snapshot threshold so a snapshot bakes
    //    with the full loop present and the active successor in the delta.
    let nudge_tx = handle.mutation_tx.clone();
    tokio::spawn(async move {
        for i in 0..40 {
            let _ = nudge_tx
                .send(GraphMutation::UpsertNode {
                    id: EntityId(format!("noise-{i}")),
                    weight: entity(&format!("Noise {i}"), EntityStatus::Active),
                })
                .await;
            if i % 8 == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    });

    // 6. Poll the WAL for the PhoenixLoop motif and capture its serialized alert.
    let mut phoenix_json: Option<String> = None;
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
                    phoenix_json = Some(result_json);
                    break;
                }
            }
        }
    }

    let _ = std::fs::remove_file(&imd_path);

    // 7a. The loop must fire — impossible unless the active→dissolved UpsertNode
    //     transition was actually applied to the existing predecessor node.
    let phoenix_json = phoenix_json.expect(
        "PhoenixLoop must fire once the predecessor's active→dissolved transition \
         is applied via UpsertNode; None here means the transition was dropped",
    );

    // 7b. The alert must carry the ward resolved from the awarded contract, plus the
    //     IMD weighting from the loaded index — end-to-end attribution.
    let v: serde_json::Value = serde_json::from_str(&phoenix_json).expect("alert JSON");
    let sb = &v["score_breakdown"];
    assert_eq!(
        sb["ward_code"].as_str(),
        Some("E05001260"),
        "alert must carry the ward from its Contract neighbour; got {phoenix_json}"
    );
    assert_eq!(
        sb["imd_rank"].as_u64(),
        Some(1),
        "alert must carry the IMD rank for its ward"
    );
    assert!(
        sb["m_env"].as_f64().unwrap_or(1.0) > 1.0,
        "IMD multiplier must be applied (m_env > 1.0 for a deprived ward); got {phoenix_json}"
    );
}
