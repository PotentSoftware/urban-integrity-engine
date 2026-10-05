//! EventPipeline: Ingest → Validate → Normalise → Insert → React → Score → Emit
//!
//! The pipeline transforms raw incoming data events into validated `GraphMutation`
//! commands ready to send to the `GraphCoordinator`. Each stage is a pure function
//! for easy unit testing and future extension.

use tracing::debug;

use urban_integrity_schema::types::edge::EdgeWeight;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::NodeWeight;

use crate::graph::types::GraphMutation;

/// Source system that produced the incoming event.
#[derive(Debug, Clone)]
pub enum EventSource {
    CompaniesHouse,
    ContractsFinder,
    PlanningPortal,
}

/// The event payload — what kind of mutation is being proposed.
#[derive(Debug, Clone)]
pub enum EventPayload {
    NodeUpsert {
        id: EntityId,
        weight: NodeWeight,
    },
    EdgeUpsert {
        src: EntityId,
        dst: EntityId,
        weight: EdgeWeight,
    },
}

/// A raw event arriving from an ingestor.
#[derive(Debug, Clone)]
pub struct IngestEvent {
    pub source: EventSource,
    pub payload: EventPayload,
}

/// Pipeline validation error.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("validation failed: {reason}")]
    ValidationFailed { reason: String },
    #[error("normalisation failed: {reason}")]
    NormalisationFailed { reason: String },
}

/// Process an `IngestEvent` through the full pipeline.
///
/// Stages: Validate → Normalise → Convert to GraphMutation.
/// Returns a `GraphMutation` ready to send to `GraphCoordinator`, or a `PipelineError`.
pub fn process(event: IngestEvent) -> Result<GraphMutation, PipelineError> {
    let event = validate(event)?;
    let event = normalise(event)?;
    let mutation = to_mutation(event);
    debug!(?mutation, "EventPipeline: mutation ready");
    Ok(mutation)
}

/// Stage 1: Validate — reject structurally invalid events before they reach the graph.
fn validate(event: IngestEvent) -> Result<IngestEvent, PipelineError> {
    match &event.payload {
        EventPayload::NodeUpsert { id, weight } => {
            if id.0.is_empty() {
                return Err(PipelineError::ValidationFailed {
                    reason: "EntityId is empty string".to_string(),
                });
            }
            if let NodeWeight::Entity(data) = weight {
                if data.name.trim().is_empty() {
                    return Err(PipelineError::ValidationFailed {
                        reason: "Entity name is empty".to_string(),
                    });
                }
            }
        }
        EventPayload::EdgeUpsert { src, dst, .. } => {
            if src.0.is_empty() {
                return Err(PipelineError::ValidationFailed {
                    reason: "Edge source EntityId is empty".to_string(),
                });
            }
            if dst.0.is_empty() {
                return Err(PipelineError::ValidationFailed {
                    reason: "Edge destination EntityId is empty".to_string(),
                });
            }
            if src.0 == dst.0 {
                return Err(PipelineError::ValidationFailed {
                    reason: "Self-loop edge rejected".to_string(),
                });
            }
        }
    }
    Ok(event)
}

/// Stage 2: Normalise — canonicalise string fields.
fn normalise(mut event: IngestEvent) -> Result<IngestEvent, PipelineError> {
    if let EventPayload::NodeUpsert { weight, .. } = &mut event.payload {
        match weight {
            NodeWeight::Entity(data) => {
                data.name = data.name.trim().to_string();
            }
            NodeWeight::Officer(data) => {
                data.name = data.name.trim().to_string();
            }
            NodeWeight::Location(data) => {
                data.name = data.name.trim().to_string();
            }
            NodeWeight::Contract(data) => {
                data.name = data.name.trim().to_string();
            }
            NodeWeight::Ward(data) => {
                data.name = data.name.trim().to_string();
            }
            NodeWeight::PoliticalActor(data) => {
                data.name = data.name.trim().to_string();
            }
            // NEW: OSINT node types bypass the ingest pipeline.
            // They are injected directly via mutation_tx by the enricher (ENR-05).
            NodeWeight::Psc(_)
            | NodeWeight::Charity(_)
            | NodeWeight::Property(_)
            | NodeWeight::OffshoreEntity(_)
            | NodeWeight::Disqualification(_) => {}
        }
    }
    Ok(event)
}

/// Stage 3: Convert a validated, normalised IngestEvent to a GraphMutation.
fn to_mutation(event: IngestEvent) -> GraphMutation {
    match event.payload {
        EventPayload::NodeUpsert { id, weight } => GraphMutation::UpsertNode { id, weight },
        EventPayload::EdgeUpsert { src, dst, weight } => {
            GraphMutation::AddEdge { src, dst, weight }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn make_entity_event(name: &str) -> IngestEvent {
        IngestEvent {
            source: EventSource::CompaniesHouse,
            payload: EventPayload::NodeUpsert {
                id: EntityId("company-001".to_string()),
                weight: NodeWeight::Entity(EntityData {
                    name: name.to_string(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            },
        }
    }

    #[test]
    fn test_pipeline_happy_path() {
        let event = make_entity_event("Test Corp");
        let mutation = process(event).unwrap();
        assert!(matches!(mutation, GraphMutation::UpsertNode { .. }));
    }

    #[test]
    fn test_pipeline_rejects_empty_id() {
        let event = IngestEvent {
            source: EventSource::CompaniesHouse,
            payload: EventPayload::NodeUpsert {
                id: EntityId(String::new()),
                weight: NodeWeight::Entity(EntityData {
                    name: "Test".to_string(),
                    company_number: None,
                    status: EntityStatus::Active,
                    incorporation_date: None,
                }),
            },
        };
        assert!(process(event).is_err());
    }

    #[test]
    fn test_pipeline_rejects_empty_name() {
        let event = make_entity_event("  ");
        assert!(process(event).is_err());
    }

    #[test]
    fn test_pipeline_rejects_self_loop() {
        use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
        let event = IngestEvent {
            source: EventSource::CompaniesHouse,
            payload: EventPayload::EdgeUpsert {
                src: EntityId("a".to_string()),
                dst: EntityId("a".to_string()),
                weight: EdgeWeight::Directs {
                    meta: EdgeMeta {
                        created_at: chrono::Utc::now(),
                        effective_from: None,
                        effective_to: None,
                    },
                    data: DirectsData {
                        details: "test".to_string(),
                        officer_role: None,
                    },
                },
            },
        };
        assert!(process(event).is_err());
    }

    #[test]
    fn test_pipeline_normalises_name() {
        let event = make_entity_event("  test corp  ");
        let mutation = process(event).unwrap();
        if let GraphMutation::UpsertNode {
            weight: NodeWeight::Entity(data),
            ..
        } = mutation
        {
            assert_eq!(data.name, "test corp");
        } else {
            panic!("Expected UpsertNode with Entity weight");
        }
    }

    #[tokio::test]
    async fn test_grph04_pipeline_to_coordinator_to_wal() {
        use crate::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
        use crate::persistence::wal::wal_channel;
        use std::time::Duration;

        let (wal_tx, wal_rx) = wal_channel(100);
        let (coordinator, handle) = GraphCoordinator::new(
            CoordinatorConfig {
                snapshot_every_n_events: 1_000_000, // never auto-snapshot in test
                snapshot_interval_secs: 3600,
            },
            wal_tx,
            None,
        );

        tokio::spawn(coordinator.run());

        // Process through pipeline → send to coordinator
        let event = make_entity_event("Acme Ltd");
        let mutation = process(event).unwrap();
        handle
            .mutation_tx
            .send(mutation)
            .await
            .expect("send mutation");

        // WAL should receive the event within 200ms
        let wal_event =
            tokio::task::spawn_blocking(move || wal_rx.recv_timeout(Duration::from_millis(200)))
                .await
                .expect("spawn_blocking");

        assert!(wal_event.is_ok(), "WAL should have received a GraphEvent");
    }
}
