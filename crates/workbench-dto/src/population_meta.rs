//! `PopulationMeta` — the D-11 "unavailable population" wire shape extended with a ward-seed
//! count, embedded in `HomeView`'s Graph card.
//!
//! Extends `status_view::PopulationView`'s 3-field shape (`count`, `generated_at`,
//! `unavailable_reason`) by one field, `ward_seed_count`, which is meaningful in both the
//! available and unavailable cases and so is always present (not wrapped in `required_option`).
//!
//! No field on this type is `#[serde(default)]`. Every field must be explicitly supplied by the
//! server-side producer — an omitted field is a serialization error the developer sees
//! immediately, not a silently-defaulted zero that looks like real data.

use serde::{Deserialize, Serialize};

/// The Home page Graph card's population wire shape. Exactly one of `count` / `unavailable_reason`
/// is `Some`; `generated_at` and `ward_seed_count` are always present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PopulationMeta {
    /// `Some(n)` when the population was computed; `None` when it was not (see
    /// `unavailable_reason`). Still a required key on the wire (no `#[serde(default)]`): an
    /// omitted key is a deserialization error, a `null` is a deliberate value.
    #[serde(deserialize_with = "required_option")]
    pub count: Option<u64>,
    /// Pre-formatted RFC3339 generation timestamp of the export/snapshot run that produced
    /// `count` (or established that it could not be produced).
    pub generated_at: String,
    /// Human-readable reason the population could not be computed; `Some` iff `count` is `None`.
    /// Still a required key on the wire (no `#[serde(default)]`).
    #[serde(deserialize_with = "required_option")]
    pub unavailable_reason: Option<String>,
    /// Number of ward seeds the reachable-from-wards walk started from — meaningful in both the
    /// available (non-zero) and unavailable (genuinely `0`, no ward code resolved) cases, so this
    /// field is always present and is NOT wrapped in `required_option`.
    pub ward_seed_count: u64,
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

    /// `PopulationMeta` with a computed count round-trips and both `count`/`unavailable_reason`
    /// remain required keys on the wire (mirrors `status_view`'s
    /// `unavailable_population_round_trips_with_reason_and_generated_at` test shape).
    #[test]
    fn available_population_round_trips_and_count_is_required() {
        let population = PopulationMeta {
            count: Some(113_250),
            generated_at: "2026-01-01T10:30:00Z".to_string(),
            unavailable_reason: None,
            ward_seed_count: 14,
        };
        let json = serde_json::to_string(&population).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["count"], serde_json::json!(113_250));
        assert!(value["unavailable_reason"].is_null());
        assert_eq!(value["ward_seed_count"], serde_json::json!(14));

        let back: PopulationMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back, population);

        let missing_count = r#"{"generated_at":"2026-01-01T10:30:00Z","unavailable_reason":null,"ward_seed_count":14}"#;
        assert!(
            serde_json::from_str::<PopulationMeta>(missing_count).is_err(),
            "an omitted `count` key must be a deserialization error, not a silent None"
        );
    }

    /// The D-11 unavailable-population shape: `count: None`, a reason present, and
    /// `ward_seed_count` genuinely `0` (no ward code resolved).
    #[test]
    fn unavailable_population_round_trips_with_zero_ward_seed_count() {
        let population = PopulationMeta {
            count: None,
            generated_at: "2026-01-01T10:30:00Z".to_string(),
            unavailable_reason: Some("no ward code resolved".to_string()),
            ward_seed_count: 0,
        };
        let json = serde_json::to_string(&population).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["count"].is_null());
        assert!(value["unavailable_reason"].is_string());
        assert_eq!(value["ward_seed_count"], serde_json::json!(0));

        let back: PopulationMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back, population);
    }
}
