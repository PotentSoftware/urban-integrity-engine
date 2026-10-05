//! `urban-integrity-workbench-dto` — the ONLY crate the browser (wasm32-unknown-unknown) is
//! allowed to depend on for typed data (D-01).
//!
//! # Contract
//!
//! - **View types only.** Every type in this crate exists to be rendered, never to enforce a
//!   domain invariant. Anything that needs to prove a construction-time guarantee (`Coverage`,
//!   `Verdict`, `PopulationSource`, ...) belongs in `crates/schema`, not here.
//! - **`Serialize`/`Deserialize` only.** No business logic, no I/O, no async, no graph traversal.
//! - **Zero native-only dependencies.** This crate's only dependency is `serde`. It must NEVER
//!   gain a dependency on `urban-integrity-schema`, `urban-integrity-core`, `surrealdb`, `tokio`,
//!   `petgraph`, or `rayon` — a later plan's CI job (WBX-03) enforces this mechanically by
//!   building this crate for `wasm32-unknown-unknown` and failing the build if any of those
//!   crates enter the dependency graph.
//!
//! Types here are named with a `*View` suffix wherever a same-named authoritative type exists in
//! `crates/schema` (e.g. `VerdictView` vs. `schema::types::verdict::Verdict`) — this lets a CI
//! grep distinguish a legitimate DTO mirror from the real invariant-carrying type reappearing
//! with a `Deserialize` derive it was never supposed to have.
//!
pub mod alerts;
pub mod algorithm;
pub mod canvas_filter;
pub mod caps;
pub mod disambiguation;
pub mod entity_profile;
pub mod entity_ref;
pub mod home_view;
pub mod investigation;
pub mod neighbourhood;
pub mod node_summary;
pub mod paging;
pub mod planted_examples;
pub mod population_meta;
pub mod provenance;
pub mod report_view;
pub mod status_view;
pub mod temporal;
pub mod verdict_view;

pub use alerts::{
    AlertDetailView, AlertFacetOptionView, AlertFacetsView, AlertPageView, AlertSummaryView,
    ImdStateView, InducedSubgraphView, PathEdgeView, PathNodeView, PopulationNoiseFloorView,
    RunAttributionView, ScoreBandView, WithdrawOutcomeView, WithdrawnMarkerView,
};
pub use algorithm::{
    AlgorithmCaps, AlgorithmOutcome, AlgorithmRequest, AlgorithmResult, CommunityResult, PathHop,
    PathResult, RankedEntity, Truncation,
};
pub use canvas_filter::{
    CanvasFilterRequest, CanvasFilterView, FacetCountView, FilterFacets, FilterRemovalCounts,
    FilteredEdgeView, MissingEvidence, WardOptionView, P_C_THRESHOLD_BANDS,
};
pub use disambiguation::{DisambiguationRowView, DisambiguationView};
pub use entity_profile::{
    AddressRowView, AlertRowView, BeneficialOwnerRowView, ContractRowView, DisqualificationRowView,
    EntityProfileDto, EntityStatusView, FindingRowView, FindingsOutcomeView, FindingsSectionView,
    IdentifierFactView, LinkedRecordRowView, LpaPresenceView, OfficerRowView, OsintAlertRowView,
    OsintLinkRowView, PlanningApplicationRowView, PropertyRowView, RelatedEntityRowView,
    SanctionsStatusView, SharedConnectionView, StatusFactView,
};
pub use entity_ref::EntityRef;
pub use home_view::{AnalysisCardView, GraphCardView, HomeView, IngestorsCardView};
pub use investigation::{
    CreateInvestigationRequest, DeleteResultView, ExpansionCutView, InvestigationDetailView,
    InvestigationSummaryView, PinNoteRequest, PinOriginView, PinRequest, PinTargetView,
    SaveSubgraphRequest, SaveSubgraphResultView, SavedInvestigationView, SavedPinView,
    UnpinRequest, UnpinResultView, UpdateInvestigationRequest,
};
pub use neighbourhood::{NeighbourhoodNodeView, NeighbourhoodView, ReportedTruncation};
pub use node_summary::NodeSummary;
pub use paging::Page;
pub use population_meta::PopulationMeta;
pub use provenance::{LinkTier, Provenance};
pub use report_view::{
    BlufView, ContradictionCheckView, EvidenceRowView, ProbeGroupView, ProbeView, ReportView,
};
pub use status_view::{AnalysisSummaryView, PopulationView, StatusView};
pub use temporal::{AsAtStatus, EffectiveRange, Temporal, NULL_RANGE_RULE, UNDATED_ROWS_ARE};
pub use verdict_view::{CoverageView, PopulationSourceView, VerdictView};
