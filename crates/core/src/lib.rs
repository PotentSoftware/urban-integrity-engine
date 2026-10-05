pub mod analysis;
pub mod config;
pub mod graph;
pub mod persistence;
pub mod pipeline;
pub mod stream;
pub mod telemetry;

pub use config::AppConfig;
pub use graph::{GraphCoordinator, GraphCoordinatorHandle, GraphMutation};
pub use persistence::{
    alert_facets, as_at_status, bfs_traversal, build_verify_verdict, classify_risk,
    detector_provenance, edge_meta_candidate, effective_range, enrich_alert_participants,
    get_alert_by_id, get_alerts_by_ids, graph_event_provenance, hydrate_from_checkpoint,
    identity_link_provenance, is_planning_application_id, is_slug_shape, is_valid_ch_number,
    iso_date, load_neighbour_index, query_alerts, query_enrichment_status,
    query_entity_by_ch_number, query_entity_by_id, query_entity_by_name, query_entity_lpa_presence,
    query_entity_neighbourhood, query_entity_neighbourhood_multi, query_entity_neighbours,
    query_related_entities, query_related_entities_complete, relation_effective_range,
    relation_provenance, resolve_entity, wal_channel, withdraw_alert, AddressRow, AdjEntry,
    AlertFacetGroups, AlertFilter, AlertRecord, AlertSort, AsAtStatus, BeneficialOwnerRow,
    ContractRow, DirectorRow, DisqualificationRow, EffectiveRange, EnrichmentStatusResult,
    EntityProfile, EntityProfileSummary, LinkTier, LpaPresence, Neighbour, NeighbourEdge,
    NeighbourIndex, NeighbourhoodNode, OsintAlertRow, OsintLink, PathStep, PlanningApplicationRow,
    PropertyRow, Provenance, RelatedEntity, RiskLevel, SanctionsStatus, SharedConnection,
    SubCandidateLinkRow, Temporal, VerifyVerdict, WithdrawOutcome, MAX_HOPS, NULL_RANGE_RULE,
    PLANNING_APPLICATION_ID_PREFIX, UNDATED_ROWS_ARE,
};
pub use pipeline::{process as process_event, IngestEvent, PipelineError};
pub use stream::{
    normalise_name, run_ch_stream, CursorStore, InMemoryCursorStore, StreamEventHandler,
    SurrealStreamCursor,
};
