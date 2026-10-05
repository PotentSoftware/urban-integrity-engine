//! `HomeView` — the workbench Home page's wire shape: the Analysis, Graph, Ingestors and Help
//! cards, each as its own sub-view so the frontend can render (or independently loading-state) one
//! card without waiting on the others.
//!
//! No field on any of these types is `#[serde(default)]`, with one deliberate exception:
//! `GraphCardView.snapshot_pending` (checkpoint fix, Phase 43 plan 09 — same rationale as
//! `status_view::StatusView.snapshot_pending`) is `#[serde(default)]` so an older frontend build
//! still parses a current server's response, defaulting to `false`. Every other field must still
//! be explicitly supplied by the server-side producer — an omitted field is a serialization error
//! the developer sees immediately, not a silently-defaulted zero that looks like real data.

use serde::{Deserialize, Serialize};

use crate::population_meta::PopulationMeta;

/// The Home page's three data-bearing cards, each independently populated. The Help card is
/// static — its only figure, the glossary term count, is computed client-side from the glossary
/// compiled into the wasm bundle, so the wire shape carries nothing for it (phase 43 code review finding WR-01:
/// the previous `help.glossary_term_count` was a server-side placeholder `0`, a fabricated value
/// on the wire).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeView {
    pub analysis: AnalysisCardView,
    pub graph: GraphCardView,
    pub ingestors: IngestorsCardView,
}

/// The Home page's Analysis card. The frontend derives "N findings confirmed" vs. "No probe run
/// yet" from whether `last_run_at` is `Some`/`None` — logic lives in a plain frontend function,
/// not a DTO-level enum, mirroring `population_headline`'s convention.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalysisCardView {
    pub analysis_name: String,
    pub database: String,
    pub findings_confirmed: u64,
    pub probes_could_not_evaluate: u64,
    pub probes_skipped: u64,
    pub total_probes: u64,
    /// `None` in the "no report yet" state; still a required key on the wire (no
    /// `#[serde(default)]`) — an omitted key is a deserialization error, a `null` is a
    /// deliberate value.
    #[serde(deserialize_with = "required_option")]
    pub last_run_at: Option<String>,
}

/// The Home page's Graph card: node/edge counts, the canonical population, and the D-06 honesty
/// line ("graph as of `<timestamp>`").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphCardView {
    pub node_count: u64,
    pub edge_count: u64,
    pub population: PopulationMeta,
    pub graph_as_of: String,
    /// `true` when a historical replay happened at boot but the CSR snapshot bake has not yet
    /// landed — `node_count`/`edge_count` are the empty initial snapshot's `0`s in that window,
    /// not a genuine empty graph (checkpoint fix, Phase 43 plan 09). `#[serde(default)]` — see
    /// this module's top-level doc comment.
    #[serde(default)]
    pub snapshot_pending: bool,
}

/// The Home page's Ingestors card: per-source last-event timestamps plus an overall rollup. Each
/// is `None` when that source has never produced an event; still a required key on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestorsCardView {
    #[serde(deserialize_with = "required_option")]
    pub companies_house_last_event: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub contracts_finder_last_event: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub planning_last_event: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub overall_last_event: Option<String>,
}

/// Copied verbatim from `status_view::required_option` (see that module's doc comment for the
/// full rationale) — do not diverge the two copies' behaviour.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_population() -> PopulationMeta {
        PopulationMeta {
            count: Some(113_250),
            generated_at: "2026-01-01T10:30:00Z".to_string(),
            unavailable_reason: None,
            ward_seed_count: 14,
        }
    }

    fn populated_home_view() -> HomeView {
        HomeView {
            analysis: AnalysisCardView {
                analysis_name: "First analysis".to_string(),
                database: "an_first_1a2b3c".to_string(),
                findings_confirmed: 3,
                probes_could_not_evaluate: 1,
                probes_skipped: 0,
                total_probes: 22,
                last_run_at: Some("2026-01-01T10:33:00Z".to_string()),
            },
            graph: GraphCardView {
                node_count: 358_484,
                edge_count: 507_179,
                population: populated_population(),
                graph_as_of: "2026-01-01T10:33:00Z".to_string(),
                snapshot_pending: false,
            },
            ingestors: IngestorsCardView {
                companies_house_last_event: Some("2026-01-01T09:00:00Z".to_string()),
                contracts_finder_last_event: Some("2026-01-01T08:30:00Z".to_string()),
                planning_last_event: Some("2026-01-01T07:15:00Z".to_string()),
                overall_last_event: Some("2026-01-01T09:00:00Z".to_string()),
            },
        }
    }

    /// `HomeView` with all four card sub-views populated round-trips.
    #[test]
    fn fully_populated_home_view_round_trips() {
        let home = populated_home_view();
        let json = serde_json::to_string(&home).expect("HomeView must serialize");
        let back: HomeView = serde_json::from_str(&json).expect("HomeView must deserialize");
        assert_eq!(back, home);
    }

    /// The Analysis card's "no report yet" state: `last_run_at: None`, counts all `0`.
    /// `last_run_at` stays a required (non-omittable) key even when `None`.
    #[test]
    fn analysis_card_no_report_yet_round_trips_and_last_run_at_is_required() {
        let mut home = populated_home_view();
        home.analysis = AnalysisCardView {
            analysis_name: "First analysis".to_string(),
            database: "an_first_1a2b3c".to_string(),
            findings_confirmed: 0,
            probes_could_not_evaluate: 0,
            probes_skipped: 0,
            total_probes: 0,
            last_run_at: None,
        };
        let json = serde_json::to_string(&home).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["analysis"]["last_run_at"].is_null());
        // `null` present as an explicit key, never omitted.
        assert!(value["analysis"]
            .as_object()
            .unwrap()
            .contains_key("last_run_at"));

        let back: HomeView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, home);

        let missing_key = r#"{"analysis_name":"First analysis","database":"an_first_1a2b3c","findings_confirmed":0,"probes_could_not_evaluate":0,"probes_skipped":0,"total_probes":0}"#;
        assert!(
            serde_json::from_str::<AnalysisCardView>(missing_key).is_err(),
            "an omitted `last_run_at` key must be a deserialization error, not a silent None"
        );
    }

    /// `GraphCardView.snapshot_pending` round-trips both `true` and `false` explicitly.
    #[test]
    fn graph_card_snapshot_pending_round_trips_true_and_false() {
        let mut home = populated_home_view();

        home.graph.snapshot_pending = true;
        let json = serde_json::to_string(&home).unwrap();
        let back: HomeView = serde_json::from_str(&json).unwrap();
        assert!(back.graph.snapshot_pending);

        home.graph.snapshot_pending = false;
        let json = serde_json::to_string(&home).unwrap();
        let back: HomeView = serde_json::from_str(&json).unwrap();
        assert!(!back.graph.snapshot_pending);
    }

    /// A `GraphCardView` payload from an older server build with no `snapshot_pending` key still
    /// deserializes, defaulting to `false` — the one deliberate `#[serde(default)]` exception
    /// this module's top-level doc comment documents.
    ///
    /// PROVE THIS CAN FAIL: removing `#[serde(default)]` from `snapshot_pending` makes this
    /// exact payload fail to deserialize with a "missing field `snapshot_pending`" error.
    #[test]
    fn graph_card_snapshot_pending_defaults_to_false_when_the_key_is_omitted() {
        let missing_key = r#"{
            "node_count": 358484,
            "edge_count": 507179,
            "population": {
                "count": 113250,
                "generated_at": "2026-01-01T10:30:00Z",
                "unavailable_reason": null,
                "ward_seed_count": 14
            },
            "graph_as_of": "2026-01-01T10:33:00Z"
        }"#;
        let graph: GraphCardView = serde_json::from_str(missing_key)
            .expect("an omitted snapshot_pending key must still deserialize");
        assert!(!graph.snapshot_pending);
    }
}
