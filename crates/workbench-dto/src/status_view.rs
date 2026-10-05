//! `StatusView`, `PopulationView`, `AnalysisSummaryView` — the wire types the workbench's status
//! page and analysis picker render (D-09, D-10, D-19).
//!
//! No field on any of these three types is `#[serde(default)]`, with one deliberate exception:
//! `StatusView.snapshot_pending` (checkpoint fix, Phase 43 plan 09) is `#[serde(default)]` so an
//! older cached/pinned frontend build — one compiled before this field existed — can still parse
//! a current server's response rather than failing outright; a missing key there defaults to
//! `false`, the pre-existing (and, before this fix, only) behaviour. Every other field must still
//! be explicitly supplied by the server-side producer — an omitted field is a serialization error
//! the developer sees immediately, not a silently-defaulted zero that looks like real data.

use serde::{Deserialize, Serialize};

/// The workbench status page's wire shape: analysis identity, graph size, the D-06 honesty line
/// ("graph as of `<timestamp>`"), the canonical population (D-19/WBX-06), whether ingestors are
/// running (always `false` this phase, but typed as `bool` rather than a hardcoded string so a
/// future phase cannot accidentally make it a lie by construction), and the D-04 actor seam.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusView {
    pub analysis_name: String,
    pub database: String,
    pub node_count: u64,
    pub edge_count: u64,
    /// Pre-formatted RFC3339 — never a bare epoch or a "now" placeholder.
    pub graph_as_of: String,
    pub population: PopulationView,
    pub ingestors_running: bool,
    /// The seam D-04 reserves for the deferred login/roles work; defaults to `"operator"` at
    /// every call site until auth ships.
    pub actor: String,
    /// `true` when a historical replay happened at boot but the CSR snapshot bake has not yet
    /// landed — `node_count`/`edge_count` above are the empty initial snapshot's `0`s in that
    /// window, not a genuine empty graph (checkpoint fix, Phase 43 plan 09). `#[serde(default)]`
    /// so an older frontend build still parses a current response — see this module's top-level
    /// doc comment for the full rationale.
    #[serde(default)]
    pub snapshot_pending: bool,
}

/// The canonical population's count and generation timestamp (D-19/WBX-06's "never one without
/// the other" rule) — there is no shape of `StatusView` that carries one without the other.
///
/// The population can legitimately be UNAVAILABLE (e.g. none of the target analysis's ward
/// codes resolve to a `Ward` node in the loaded graph, so the reachable-from-wards walk has no
/// seeds). That state is carried explicitly — `count: None` plus `unavailable_reason: Some(..)`
/// — rather than as a fabricated `0` that a reader would take for a real, empty population.
/// Exactly one of `count` / `unavailable_reason` is `Some`; `generated_at` is always present.
/// Both `Option`s are still required keys on the wire (no `#[serde(default)]`): an omitted key
/// is a deserialization error, a `null` is a deliberate value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PopulationView {
    /// `Some(n)` when the population was computed; `None` when it was not (see
    /// `unavailable_reason`).
    #[serde(deserialize_with = "required_option")]
    pub count: Option<u64>,
    /// Pre-formatted RFC3339 generation timestamp of the export/snapshot run that produced
    /// `count` (or established that it could not be produced).
    pub generated_at: String,
    /// Human-readable reason the population could not be computed; `Some` iff `count` is `None`.
    #[serde(deserialize_with = "required_option")]
    pub unavailable_reason: Option<String>,
}

/// Serde's derive maps a MISSING `Option` field to `None` on its own — which would let a
/// producer drop `count`/`unavailable_reason` entirely and have the UI read the omission as a
/// deliberate `null`. Routing the field through this deserializer switches that special case
/// off: an omitted key is a "missing field" error, only an explicit `null` is `None`. This is
/// the `Option`-field form of this module's "no `#[serde(default)]` anywhere" rule.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// One entry in the analysis picker's read-only `_catalog` listing (D-10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalysisSummaryView {
    pub db_name: String,
    pub label: String,
    pub is_current: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generic fixture values only — never a real analysis's name, database id or live-corpus
    /// counts (D-10 amended: no analysis-specific literal anywhere in the UI or its fixtures;
    /// enforced by `scripts/check-no-analysis-literals.sh`, which scans this crate).
    fn sample_status() -> StatusView {
        StatusView {
            analysis_name: "First analysis".to_string(),
            database: "an_first_1a2b3c".to_string(),
            node_count: 12_345,
            edge_count: 23_456,
            graph_as_of: "2026-01-01T10:33:00Z".to_string(),
            population: PopulationView {
                count: Some(1_234),
                generated_at: "2026-01-01T10:30:00Z".to_string(),
                unavailable_reason: None,
            },
            ingestors_running: false,
            actor: "operator".to_string(),
            snapshot_pending: false,
        }
    }

    /// The concrete, checkable proof of D-04's "carries an actor field defaulting to 'operator'
    /// from day one" — asserts the literal `"actor"` key is present in the serialized JSON with
    /// the value `"operator"`, not merely that the struct round-trips (a renamed field would
    /// still round-trip through itself but would break every other consumer expecting `"actor"`
    /// on the wire).
    ///
    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`'s "Proving a Test Can Fail"): this test was
    /// run against a version of `StatusView` with `actor` marked `#[serde(rename = "actor_name")]`
    /// — the assertion failed (`value["actor"]` resolved to `Null` instead of `"operator"`, the
    /// field was present at `"actor_name"` instead) — before the rename was removed to the version
    /// committed here.
    #[test]
    fn actor_field_round_trips_as_operator_by_default() {
        let status = sample_status();
        let json = serde_json::to_string(&status).expect("StatusView must serialize");

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["actor"], serde_json::json!("operator"));

        let back: StatusView = serde_json::from_str(&json).expect("StatusView must deserialize");
        assert_eq!(back, status);
        assert_eq!(back.actor, "operator");
    }

    /// `population`'s `count` and `generated_at` always travel together — there is no shape of
    /// `StatusView` where one is present without the other.
    #[test]
    fn population_count_and_generated_at_travel_together() {
        let status = sample_status();
        let json = serde_json::to_string(&status).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(value["population"]["count"].is_u64());
        assert!(value["population"]["generated_at"].is_string());
        assert_eq!(value["population"]["count"], serde_json::json!(1_234));
        assert_eq!(
            value["population"]["generated_at"],
            serde_json::json!("2026-01-01T10:30:00Z")
        );
        assert!(value["population"]["unavailable_reason"].is_null());
    }

    /// An unavailable population is an explicit wire state — `count: null` with a reason and
    /// the generation timestamp still present — and round-trips. An omitted `count`/
    /// `unavailable_reason` key is a deserialization ERROR (no `#[serde(default)]`), so a
    /// producer cannot drop the field and have the UI silently read it as "unknown".
    #[test]
    fn unavailable_population_round_trips_with_reason_and_generated_at() {
        let population = PopulationView {
            count: None,
            generated_at: "2026-01-01T10:30:00Z".to_string(),
            unavailable_reason: Some("no ward code resolved to a Ward node".to_string()),
        };
        let json = serde_json::to_string(&population).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["count"].is_null());
        assert!(value["unavailable_reason"].is_string());
        assert!(value["generated_at"].is_string());

        let back: PopulationView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, population);

        let missing_key = r#"{"generated_at":"2026-01-01T10:30:00Z","unavailable_reason":null}"#;
        assert!(
            serde_json::from_str::<PopulationView>(missing_key).is_err(),
            "an omitted `count` key must be a deserialization error, not a silent None"
        );
    }

    /// `ingestors_running` is typed as a real `bool`, not a hardcoded string — this phase always
    /// sets it `false`, but the field can carry a genuine future value without a shape change.
    #[test]
    fn ingestors_running_is_a_real_bool_not_a_hardcoded_string() {
        let status = sample_status();
        assert!(!status.ingestors_running);

        let json = serde_json::to_string(&status).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ingestors_running"], serde_json::json!(false));
    }

    /// `snapshot_pending` round-trips both `true` and `false` explicitly.
    #[test]
    fn snapshot_pending_round_trips_true_and_false() {
        let mut status = sample_status();

        status.snapshot_pending = true;
        let json = serde_json::to_string(&status).unwrap();
        let back: StatusView = serde_json::from_str(&json).unwrap();
        assert!(back.snapshot_pending);

        status.snapshot_pending = false;
        let json = serde_json::to_string(&status).unwrap();
        let back: StatusView = serde_json::from_str(&json).unwrap();
        assert!(!back.snapshot_pending);
    }

    /// A payload from an older server build (or an older fixture) with no `snapshot_pending` key
    /// at all still deserializes, defaulting to `false` — the one deliberate `#[serde(default)]`
    /// exception this module's top-level doc comment documents.
    ///
    /// PROVE THIS CAN FAIL: removing `#[serde(default)]` from `snapshot_pending` makes this
    /// exact payload fail to deserialize with a "missing field `snapshot_pending`" error.
    #[test]
    fn snapshot_pending_defaults_to_false_when_the_key_is_omitted() {
        let missing_key = r#"{
            "analysis_name": "First analysis",
            "database": "an_first_1a2b3c",
            "node_count": 12345,
            "edge_count": 23456,
            "graph_as_of": "2026-01-01T10:33:00Z",
            "population": {
                "count": 1234,
                "generated_at": "2026-01-01T10:30:00Z",
                "unavailable_reason": null
            },
            "ingestors_running": false,
            "actor": "operator"
        }"#;
        let status: StatusView = serde_json::from_str(missing_key)
            .expect("an omitted snapshot_pending key must still deserialize");
        assert!(!status.snapshot_pending);
    }

    /// `AnalysisSummaryView` round-trips its three fields for the analysis picker (D-10).
    #[test]
    fn analysis_summary_view_round_trips() {
        let entry = AnalysisSummaryView {
            db_name: "an_second_4d5e6f".to_string(),
            label: "Second analysis".to_string(),
            is_current: true,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: AnalysisSummaryView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }
}
