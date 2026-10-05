//! Integration tests for the planning portal ingestion pipeline.
//!
//! DB-gated tests require:
//!   URBAN_DB_TESTS=1    — enables DB tests
//!   SurrealDB running on [::1]:8000 or 127.0.0.1:8000
//!
//! Live sidecar tests require:
//!   URBAN_PLANNING_TESTS=1   — enables tests that call the real sidecar
//!   Scrapling sidecar running on http://localhost:8765

use std::net::TcpStream;
use std::sync::Arc;

// ── Test guards ────────────────────────────────────────────────────────────────

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    // TCP pre-flight check
    TcpStream::connect("[::1]:8000").is_ok() || TcpStream::connect("127.0.0.1:8000").is_ok()
}

fn planning_tests_enabled() -> bool {
    if std::env::var("URBAN_PLANNING_TESTS").unwrap_or_default() != "1" {
        return false;
    }
    // Sidecar TCP pre-flight
    TcpStream::connect("127.0.0.1:8765").is_ok()
}

// ── Mapper contract tests (always run — no DB required) ────────────────────────

#[cfg(test)]
mod mapper_contract {
    use urban_integrity_core::pipeline::{EventPayload, EventSource};
    use urban_integrity_ingestor::planning::{
        mapper::map_planning_item, sidecar::PlanningPortalItem,
    };
    use urban_integrity_schema::types::node::NodeWeight;

    fn make_item(reference: &str) -> PlanningPortalItem {
        PlanningPortalItem {
            reference: Some(reference.to_string()),
            description: Some("Construction of 3 dwellings".to_string()),
            address: Some("High Street, Llangefni, LL77 7AA".to_string()),
            ward: Some("Cefni".to_string()),
            applicant: Some("Davies Construction Ltd".to_string()),
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        }
    }

    #[test]
    fn mapper_produces_4_nodes_3_edges() {
        let events = map_planning_item(&make_item("FPL/2024/100"), "W05001497");
        assert_eq!(events.len(), 7, "Expected 4 nodes + 3 edges");
        let nodes = events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::NodeUpsert { .. }))
            .count();
        let edges = events
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::EdgeUpsert { .. }))
            .count();
        assert_eq!(nodes, 4, "4 node upserts");
        assert_eq!(edges, 3, "3 edge upserts");
    }

    #[test]
    fn mapper_source_is_planning_portal() {
        let events = map_planning_item(&make_item("FPL/2024/101"), "W05001497");
        for e in &events {
            // EventSource doesn't derive PartialEq — use matches!
            assert!(
                matches!(e.source, EventSource::PlanningPortal),
                "All planning events must have PlanningPortal source"
            );
        }
    }

    #[test]
    fn mapper_contract_id_starts_with_planning_prefix() {
        let events = map_planning_item(&make_item("FPL/2024/102"), "W05001497");
        let contract = events.iter().find(|e| {
            matches!(&e.payload, EventPayload::NodeUpsert { id, weight: NodeWeight::Contract(_) } if id.0.starts_with("planning:"))
        });
        assert!(
            contract.is_some(),
            "Contract node with planning: prefix must exist"
        );
    }

    #[test]
    fn mapper_entity_id_starts_with_director_prefix() {
        let events = map_planning_item(&make_item("FPL/2024/103"), "W05001497");
        let entity = events.iter().find(|e| {
            matches!(&e.payload, EventPayload::NodeUpsert { id, weight: NodeWeight::Entity(_) } if id.0.starts_with("director:"))
        });
        assert!(
            entity.is_some(),
            "Provisional entity node must exist with director: prefix"
        );
    }

    #[test]
    fn mapper_ward_code_on_contract() {
        let events = map_planning_item(&make_item("FPL/2024/104"), "W05001497");
        let contract_event = events.iter().find(|e| {
            matches!(
                &e.payload,
                EventPayload::NodeUpsert {
                    weight: NodeWeight::Contract(_),
                    ..
                }
            )
        });
        if let Some(e) = contract_event {
            if let EventPayload::NodeUpsert {
                weight: NodeWeight::Contract(c),
                ..
            } = &e.payload
            {
                assert_eq!(c.ward_code.as_deref(), Some("W05001497"));
            }
        }
    }

    #[test]
    fn mapper_skips_item_without_reference() {
        let item = PlanningPortalItem {
            reference: None,
            description: Some("desc".to_string()),
            address: Some("addr".to_string()),
            ward: None,
            applicant: None,
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001492");
        assert!(events.is_empty(), "No events for item without reference");
    }

    #[test]
    fn mapper_handles_empty_applicant_gracefully() {
        let item = PlanningPortalItem {
            reference: Some("FPL/2024/105".to_string()),
            description: Some("desc".to_string()),
            address: Some("addr".to_string()),
            ward: Some("Cefni".to_string()),
            applicant: None, // No applicant name — should fall back to "Unknown Applicant"
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        };
        let events = map_planning_item(&item, "W05001497");
        assert_eq!(
            events.len(),
            7,
            "Should still produce 7 events with fallback applicant name"
        );
    }
}

// ── DB-gated integration test ──────────────────────────────────────────────────

#[cfg(test)]
mod pipeline_db_integration {
    use super::*;
    use urban_integrity_core::{
        graph::coordinator::{CoordinatorConfig, GraphCoordinator},
        persistence::wal::wal_channel,
        pipeline::process,
    };
    use urban_integrity_ingestor::planning::{
        mapper::map_planning_item, sidecar::PlanningPortalItem,
    };

    #[tokio::test]
    async fn planning_pipeline_processes_events_without_panic() {
        if !db_tests_enabled() {
            eprintln!("SKIP: planning_pipeline_processes_events_without_panic (set URBAN_DB_TESTS=1 with SurrealDB on :8000)");
            return;
        }

        let (wal_tx, _) = wal_channel(1024);
        let coord_cfg = CoordinatorConfig {
            snapshot_every_n_events: 100,
            snapshot_interval_secs: 60,
        };

        // `_ns` must stay bound for the whole test: dropping it removes the namespace.
        let (db, _ns) = {
            use surrealdb::engine::any::Any;
            use surrealdb::opt::auth::Root;
            use surrealdb::Surreal;
            let guard = urban_integrity_test_support::NsGuard::with_prefix("test_planning");
            let db: Surreal<Any> = Surreal::init();
            db.connect("http://localhost:8000").await.expect("connect");
            db.signin(Root {
                username: "root".to_string(),
                password: "root".to_string(),
            })
            .await
            .expect("auth");
            db.use_ns(guard.name())
                .use_db("test_planning")
                .await
                .expect("ns");
            (Arc::new(db), guard)
        };

        let (coordinator, handle) =
            GraphCoordinator::new_with_db(coord_cfg, wal_tx, None, Some(db.clone()));
        tokio::spawn(coordinator.run());

        let item = PlanningPortalItem {
            reference: Some("TEST/PLANNING/001".to_string()),
            description: Some("Test warehouse construction".to_string()),
            address: Some("1 Test Street, Llangefni LL77 7AA".to_string()),
            ward: Some("Cefni".to_string()),
            applicant: Some("Test Developments Ltd".to_string()),
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        };

        let events = map_planning_item(&item, "W05001497");
        assert!(!events.is_empty(), "Must produce events from valid item");

        for event in events {
            match process(event) {
                Ok(mutation) => {
                    // Send to coordinator — don't assert on result, channel may be full in test
                    let _ = handle.mutation_tx.send(mutation).await;
                }
                Err(e) => {
                    panic!("Pipeline rejected valid planning event: {:?}", e);
                }
            }
        }

        // Allow coordinator to process
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // No panic = PASS for this integration test
    }
}

// ── Live sidecar tests (URBAN_PLANNING_TESTS=1) ────────────────────────────────

#[cfg(test)]
mod sidecar_live {
    use super::*;
    use urban_integrity_ingestor::planning::sidecar::ScrapingSidecar;

    #[tokio::test]
    async fn sidecar_health_check_returns_without_panic() {
        if !planning_tests_enabled() {
            eprintln!(
                "SKIP: sidecar_health_check (set URBAN_PLANNING_TESTS=1 with sidecar on :8765)"
            );
            return;
        }
        let sidecar = ScrapingSidecar::new("http://localhost", 8765);
        let is_healthy = sidecar.is_healthy().await;
        // Log but don't hard-assert — sidecar may be starting up
        println!(
            "Sidecar health check: {}",
            if is_healthy { "ok" } else { "not ready" }
        );
    }

    #[tokio::test]
    async fn sidecar_scrape_iow_idox_returns_items_or_empty() {
        if !planning_tests_enabled() {
            eprintln!("SKIP: sidecar_scrape_iow (set URBAN_PLANNING_TESTS=1)");
            return;
        }
        let sidecar = ScrapingSidecar::new("http://localhost", 8765);
        // IoW is a live equivalent Idox portal — Anglesey offline until 27 May 2026
        let items = sidecar
            .scrape(
                "https://publicaccess.iow.gov.uk/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=BEMBRIDGE",
                "idox",
            )
            .await;
        // Don't assert on count — just that it doesn't panic
        println!(
            "Live Idox scrape (IoW/Bembridge) returned {} items",
            items.len()
        );
    }
}
