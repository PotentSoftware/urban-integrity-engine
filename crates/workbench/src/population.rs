//! `GET /api/graph/population/meta` — D-20's `PopulationMeta` wire shape, layered onto
//! `workbench_routes` in `crates/workbench::boot::build_router` (Phase 43 plan 05).
//!
//! Reads exclusively from `Extension<Arc<WorkbenchState>>` — no client-supplied query/path
//! parameter selects the analysis (the same Extension-only discipline `status.rs` documents).

use std::sync::Arc;

use axum::{response::Json, Extension};

use urban_integrity_workbench_dto::PopulationMeta;

use crate::boot::{PopulationSummary, WorkbenchState};

/// Maps the boot-time [`PopulationSummary`] onto the wire shape, mirroring `status::
/// population_view` exactly and additionally carrying `ward_seed_count` — the number of ward
/// seeds the reachable-from-wards walk actually started from, threaded through
/// `CanonicalPopulation`/`PopulationSummary` from `crates/cli`'s canonical-population pipeline
/// rather than re-derived independently in this crate.
pub(crate) fn population_meta_view(population: &PopulationSummary) -> PopulationMeta {
    match population {
        PopulationSummary::Available {
            count,
            generated_at,
            ward_seed_count,
        } => PopulationMeta {
            count: Some(*count),
            generated_at: generated_at.to_rfc3339(),
            unavailable_reason: None,
            ward_seed_count: *ward_seed_count,
        },
        PopulationSummary::Unavailable {
            reason,
            generated_at,
            ward_seed_count,
        } => PopulationMeta {
            count: None,
            generated_at: generated_at.to_rfc3339(),
            unavailable_reason: Some(reason.clone()),
            ward_seed_count: *ward_seed_count,
        },
    }
}

/// `GET /api/graph/population/meta` — the D-20 `PopulationMeta` shape the Home page's Graph card
/// consumes.
pub async fn population_meta_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Json<PopulationMeta> {
    Json(population_meta_view(&state.population))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// An available population maps `count: Some(n)`, no reason, and the threaded ward-seed
    /// count — never a fabricated `0` count for `unavailable_reason`.
    #[test]
    fn available_population_maps_count_and_ward_seed_count() {
        let generated_at = Utc::now();
        let view = population_meta_view(&PopulationSummary::Available {
            count: 358_484,
            generated_at,
            ward_seed_count: 17,
        });
        assert_eq!(view.count, Some(358_484));
        assert_eq!(view.unavailable_reason, None);
        assert_eq!(view.ward_seed_count, 17);
        assert_eq!(view.generated_at, generated_at.to_rfc3339());
    }

    /// D-11's unavailable-population shape: `count: None`, a reason present, and
    /// `ward_seed_count` genuinely `0` — never a fabricated zero-as-available.
    ///
    /// PROVE THIS CAN FAIL: mapping `Unavailable` to `count: Some(0)` fails the first assertion.
    #[test]
    fn unavailable_population_is_none_with_a_reason_not_a_fabricated_zero() {
        let generated_at = Utc::now();
        let view = population_meta_view(&PopulationSummary::Unavailable {
            reason: "no ward code resolved".to_string(),
            generated_at,
            ward_seed_count: 0,
        });
        assert_eq!(view.count, None);
        assert_eq!(
            view.unavailable_reason.as_deref(),
            Some("no ward code resolved")
        );
        assert_eq!(view.ward_seed_count, 0);
        assert_eq!(view.generated_at, generated_at.to_rfc3339());
    }
}
