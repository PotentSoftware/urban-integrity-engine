//! Wire contract for the Phase 46 alert / motif triage inbox (TRI-01, TRI-02).
//!
//! Every type here is the *exact* shape the `/api/alerts` list/detail/withdraw handlers
//! (`crates/workbench`, native) produce and the `/alerts` / `/alert/:id` Leptos pages
//! (`crates/workbench-web`, wasm) consume. This is an interface-first module: at the time it
//! was written, no handler, no renderer and no page existed yet — every later Phase 46 plan
//! (46-06 handlers, 46-07 renderer, 46-08 row/filters, 46-09 list page, 46-10 detail page)
//! imports these types verbatim rather than inventing a second partial guess at the same JSON
//! shape (the "scavenger hunt" anti-pattern the phase 46 design notes' objective names explicitly).
//!
//! Like every module in this crate: no `chrono`, no `petgraph`, no `surrealdb`, no `tokio` —
//! timestamps cross the wire as RFC 3339 strings, edges cross as display-name strings resolved
//! server-side from `csr_view::edge_type_name`, and every enum tag follows this crate's
//! `#[serde(tag = "...", rename_all = "snake_case")]` convention. No field is
//! `#[serde(default)]` — an omitted key is a deserialization error, not a silently-defaulted
//! empty value, per this crate's blanket "no default" rule (see `entity_ref.rs`).
//!
//! # The governing principle (the phase 46 design notes § Specific Ideas)
//!
//! A missing thing must never look like a neutral thing. Four of the fourteen operator decisions
//! are this same rule in four places, and each is a distinct variant/state below rather than a
//! null or an `Option::None` collapsing two different facts into one shape:
//!
//! * D-02 — [`InducedSubgraphView::NoEdges`] states the absence of a path outright, with the
//!   snapshot's generation timestamp, rather than falling back to a flat participant-chip list
//!   that looks identical to a probe that never had structure.
//! * D-05 — [`RunAttributionView`]'s `first_seen_run_id`/`last_seen_run_id` are both
//!   `Option<String>`, `None` meaning "run not recorded" for every pre-existing alert, never a
//!   guessed singleton.
//! * D-07 — [`ImdStateView`] has five variants, not a null `imd_rank` collapsing "scored
//!   neutral" and "no index loaded" into the same shape.
//! * D-10 — [`AlertPageView::withdrawn_hidden_count`] states how many alerts are hidden behind
//!   the withdrawn toggle, rather than silently filtering them with no count.

use crate::entity_ref::EntityRef;
use serde::{Deserialize, Serialize};

/// The `P_c` score band a row's badge colour follows — the wire mirror of whatever banding the
/// server already applies to `AlertRecord.score` (`crates/core::persistence::queries`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScoreBandView {
    Critical,
    High,
    Medium,
    BelowMedium,
}

/// The five honest states the `m_env`/IMD field can be in (D-07, the phase 46 research notes' correction to
/// D-07's original three-state naming). A null `imd_rank` alone cannot distinguish "scored
/// neutral" from "no index was loaded for this run" — keeping five distinct variants is what
/// prevents that collapse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImdStateView {
    /// A deprivation index was loaded and the alert's ward matched a row in it.
    Scored { imd_rank: u32, m_env: f64 },
    /// A deprivation index was loaded but the alert's ward code is not covered by it (e.g. an
    /// English ward against the shipped Welsh WIMD 2019 index).
    WardNotCovered { imd_csv_path: String, m_env: f64 },
    /// No deprivation index was loaded for the run that produced this alert (`data/*` is
    /// gitignored, so this is the default state on a fresh clone).
    NoIndexLoaded { m_env: f64 },
    /// This alert's detector has no `ScoreBreakdown` concept at all — `PropertyAccumulationAnomaly`
    /// and `DonationContractCorrelation` (see plan 46-03's correction to the phase 46 research notes'
    /// participant-count table).
    NotApplicable,
    /// This alert predates run-id stamping (D-05) and its `ScoreBreakdown` was never captured.
    NotRecorded,
}

/// Which run first produced this alert and which run most recently re-confirmed it (D-05, D-06).
/// Detection UPSERTs, so a re-firing motif overwrites its own row — carrying both first-seen and
/// last-seen makes staleness legible without a second query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunAttributionView {
    pub first_seen_run_id: Option<String>,
    pub first_seen_at: Option<String>,
    pub last_seen_run_id: Option<String>,
    pub last_seen_at: Option<String>,
    /// How many of the motif type's own runs have happened since this alert was last
    /// re-confirmed. `None` when not recorded (pre-run-stamping) or when this alert's
    /// `last_seen_run_id` is the motif type's own most recent run (i.e. not stale).
    pub runs_since_last_seen: Option<u32>,
}

/// The withdrawal record on an alert that has been withdrawn (D-08, D-10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WithdrawnMarkerView {
    pub withdrawn_at: String,
    pub withdrawn_reason: String,
}

/// One node in a drawn induced subgraph. `degree_in_subgraph` is the sort key the renderer's
/// node-selection order needs (UI-SPEC's motif-path renderer contract: "degree within the
/// induced subgraph descending, then entity id ascending").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathNodeView {
    pub entity: EntityRef,
    pub degree_in_subgraph: u32,
}

/// One edge in a drawn induced subgraph. `edge_type` is a `csr_view::edge_type_name` display
/// string, not the `EdgeWeight` enum — `crate::alerts` has no `petgraph` dependency, so a typed
/// edge crosses the wire pre-resolved to its display name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathEdgeView {
    pub from_id: String,
    pub to_id: String,
    pub edge_type: String,
}

/// The induced subgraph over an alert's participants, resolved live from the CSR snapshot (D-01).
/// Three variants rather than an empty `Drawable` for the no-edges case, per the governing
/// principle: an absent path is stated outright (D-02), never rendered indistinguishably from a
/// path that simply has zero edges by construction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InducedSubgraphView {
    /// Edges exist and are drawn, budgeted server-side to whichever cap the caller requested
    /// (`caps::ALERT_PATH_NODE_BUDGET_ROW` for a row thumbnail, `caps::ALERT_PATH_NODE_BUDGET_DETAIL`
    /// for the detail route).
    Drawable {
        nodes: Vec<PathNodeView>,
        edges: Vec<PathEdgeView>,
        participant_total: usize,
        drawn_total: usize,
    },
    /// No connecting edges remain in the current population (D-02). The alert stays listed; the
    /// path slot names the absence outright, carrying the snapshot's generation timestamp so the
    /// staleness is legible rather than looking like a detector that never had structure.
    NoEdges {
        participant_total: usize,
        snapshot_generated_at: String,
    },
    /// `MotifKind::SingleSubject` — one entity, no path to draw at all (plan 46-03).
    SingleParticipant { entity: EntityRef },
}

/// The row shape — one alert as it renders in the `/alerts` list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertSummaryView {
    pub id: String,
    /// The wire tag of the `MotifType` variant that produced this alert (e.g. `"AddressCluster"`).
    pub motif_type: String,
    pub detector_display_name: String,
    pub score: f64,
    pub score_band: ScoreBandView,
    pub imd: ImdStateView,
    pub ward_code: Option<String>,
    pub detected_at: String,
    pub withdrawn: Option<WithdrawnMarkerView>,
    pub run: RunAttributionView,
    pub participant_total: usize,
    /// Server-truncated to `caps::ALERT_ROW_PARTICIPANT_CHIPS`.
    pub participant_chips: Vec<EntityRef>,
    /// Server-budgeted to `caps::ALERT_PATH_NODE_BUDGET_ROW`.
    pub path: InducedSubgraphView,
}

/// One facet option (a detector or a ward) with its live, other-filters-respecting match count
/// (D-13).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertFacetOptionView {
    pub value: String,
    pub label: String,
    pub count: usize,
}

/// The facet panel for the `/alerts` list (D-13). Facet values are enumerated from the population
/// itself so a name mismatch (the `normalize_motif_type` incident the phase 46 design notes document) is
/// structurally impossible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertFacetsView {
    /// All 8 `MotifType` variants, always present, `count` respecting every other active filter.
    pub detectors: Vec<AlertFacetOptionView>,
    /// Sorted count-desc then name-asc.
    pub wards: Vec<AlertFacetOptionView>,
    /// The D-13 footnote figure — how many matching alerts carry no ward code at all.
    pub no_ward_code_count: usize,
}

/// D-11's server-measured population-composition line — every figure computed at request time,
/// never hardcoded (per A-01 in the phase 46 assumptions notes; `cluster.rs:39`'s 71.5% figure is a
/// point-in-time measurement, not a constant this view may bake in).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PopulationNoiseFloorView {
    pub total: usize,
    pub floor_motif_type: String,
    pub floor_detector_display_name: String,
    pub floor_count: usize,
    pub floor_percent: f64,
    pub floor_participant_count: usize,
}

/// The full `/alerts` list response: one page of alerts, the facet panel, the population-noise
/// line, and the counts D-10/D-12 require to be stated rather than silently applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertPageView {
    pub alerts: Vec<AlertSummaryView>,
    pub total_matched: usize,
    pub truncated: bool,
    /// How many matching alerts are withdrawn and hidden behind the D-10 toggle.
    pub withdrawn_hidden_count: usize,
    pub facets: AlertFacetsView,
    pub population_note: PopulationNoiseFloorView,
    /// `false` drives the "no detector run is recorded" empty-state addendum.
    pub detector_run_recorded: bool,
}

/// The `/alert/:id` detail response. `summary` is reused for every shared field — the detail
/// route never re-derives `score_band`/`imd`/`run` independently of the row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertDetailView {
    pub summary: AlertSummaryView,
    /// Budgeted to `caps::ALERT_PATH_NODE_BUDGET_DETAIL`.
    pub path_detail: InducedSubgraphView,
    /// Capped at `caps::ALERT_PARTICIPANTS_LIST_CAP` — independent of the SVG drawing budget
    /// above (UI-SPEC: "coverage is not lost: the Participants list below names every
    /// participant").
    pub participants: Vec<EntityRef>,
    pub participants_truncated: bool,
}

/// The wire mirror of the `WithdrawOutcome` enum `crates/core` gains (plan 46-02), returned by
/// the withdraw endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WithdrawOutcomeView {
    Withdrawn { hidden_count: usize },
    AlreadyWithdrawn { at: String, reason: String },
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entity(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: "Jane Smith".to_string(),
            kind: "Entity".to_string(),
        }
    }

    /// Every `ImdStateView` variant round-trips through `serde_json::to_string`/`from_str`
    /// unchanged.
    #[test]
    fn imd_state_view_variants_round_trip() {
        let variants = vec![
            ImdStateView::Scored {
                imd_rank: 42,
                m_env: 0.73,
            },
            ImdStateView::WardNotCovered {
                imd_csv_path: "data/wimd_2019_wards.csv".to_string(),
                m_env: 1.0,
            },
            ImdStateView::NoIndexLoaded { m_env: 1.0 },
            ImdStateView::NotApplicable,
            ImdStateView::NotRecorded,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).expect("ImdStateView must serialize");
            let back: ImdStateView =
                serde_json::from_str(&json).expect("ImdStateView must deserialize");
            assert_eq!(back, variant, "round-trip mismatch for {json}");
        }
    }

    /// Every `InducedSubgraphView` variant round-trips through `serde_json`.
    #[test]
    fn induced_subgraph_view_variants_round_trip() {
        let variants = vec![
            InducedSubgraphView::Drawable {
                nodes: vec![PathNodeView {
                    entity: sample_entity("director:jane-smith"),
                    degree_in_subgraph: 3,
                }],
                edges: vec![PathEdgeView {
                    from_id: "director:jane-smith".to_string(),
                    to_id: "company:12345678".to_string(),
                    edge_type: "IsDirectorOf".to_string(),
                }],
                participant_total: 5,
                drawn_total: 1,
            },
            InducedSubgraphView::NoEdges {
                participant_total: 5,
                snapshot_generated_at: "2026-09-21T00:00:00Z".to_string(),
            },
            InducedSubgraphView::SingleParticipant {
                entity: sample_entity("director:jane-smith"),
            },
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).expect("InducedSubgraphView must serialize");
            let back: InducedSubgraphView =
                serde_json::from_str(&json).expect("InducedSubgraphView must deserialize");
            assert_eq!(back, variant, "round-trip mismatch for {json}");
        }
    }

    /// Every `WithdrawOutcomeView` variant round-trips through `serde_json`.
    #[test]
    fn withdraw_outcome_view_variants_round_trip() {
        let variants = vec![
            WithdrawOutcomeView::Withdrawn { hidden_count: 3 },
            WithdrawOutcomeView::AlreadyWithdrawn {
                at: "2026-09-20T12:00:00Z".to_string(),
                reason: "confirmed false positive".to_string(),
            },
            WithdrawOutcomeView::NotFound,
        ];
        for variant in variants {
            let json = serde_json::to_string(&variant).expect("WithdrawOutcomeView must serialize");
            let back: WithdrawOutcomeView =
                serde_json::from_str(&json).expect("WithdrawOutcomeView must deserialize");
            assert_eq!(back, variant, "round-trip mismatch for {json}");
        }
    }

    /// `rename_all = "snake_case"` is actually applied to `ScoreBandView`, not just declared.
    #[test]
    fn score_band_view_critical_serialises_to_snake_case_literal() {
        let json = serde_json::to_string(&ScoreBandView::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
    }
}
