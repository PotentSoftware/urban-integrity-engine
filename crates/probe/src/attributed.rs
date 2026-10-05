//! `Attributed` -- D1's unconstructible-unattributed guarantee (VRD-08).
//!
//! Every OSINT evidence count a Tier-2 probe reports must be wrapped in an [`Attributed`] value
//! -- there is no path to a bare `u64` "evidence count" that skipped the registry. The refusal
//! mechanism lives in [`count_for_source`]/[`from_rows`]: a `term` with more than one registered
//! producer and no [`EvidenceTerm::discriminator`] cannot be attributed to a single named
//! `source`, because there is no way to tell which producer's rows actually satisfy it. This is
//! what makes VRD-08's closure structural rather than a per-probe audit that "re-opens on probe
//! 13" (phase 33 design decision D1).
//!
//! Layered directly on top of `urban_integrity_core::persistence::scan`'s
//! `scan_events_by_type`/`scan_events_by_payload_contains` -- this module never re-implements the
//! underlying SurrealQL, only the attribution/refusal logic around it.

use std::collections::HashMap;

use serde_json::Value;
use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::attribution::{EvidenceTerm, REGISTRY};

/// Errors from constructing an [`Attributed`] value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributionError {
    /// `term` has more than one registered producer and no discriminator to narrow to a single
    /// `source` -- D1's core refusal guarantee.
    MultiProducerNoDiscriminator {
        term: &'static str,
        producers: &'static [&'static str],
    },
    /// `term` is not present in [`REGISTRY`] at all.
    UnknownTerm(&'static str),
    /// The underlying `graph_events` scan failed.
    ScanFailed(String),
}

impl std::fmt::Display for AttributionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttributionError::MultiProducerNoDiscriminator { term, producers } => write!(
                f,
                "term {term:?} has {} registered producers ({producers:?}) and no discriminator \
                 -- cannot attribute to a single source",
                producers.len()
            ),
            AttributionError::UnknownTerm(term) => {
                write!(f, "term {term:?} is not registered in REGISTRY")
            }
            AttributionError::ScanFailed(e) => write!(f, "attribution scan failed: {e}"),
        }
    }
}

impl std::error::Error for AttributionError {}

/// An evidence count that provably came from a named, registered producer (or set of producers)
/// -- never a bare unattributed number. `producers` carries the per-producer breakdown when
/// known, a single `(source, value)` tuple otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct Attributed {
    pub value: u64,
    pub term: &'static str,
    pub producers: Vec<(&'static str, u64)>,
}

fn lookup(term: &'static str) -> Result<&'static EvidenceTerm, AttributionError> {
    REGISTRY
        .iter()
        .find(|e| e.term == term)
        .ok_or(AttributionError::UnknownTerm(term))
}

/// The `weight_json` payload of one `graph_events` row, parsed. `weight_json` is stored as a
/// JSON-encoded string (`scan.rs`'s convention throughout), never a nested object, so this always
/// goes through `serde_json::from_str`.
fn weight_of(row: &Value) -> Option<Value> {
    let wj = row["payload"]["weight_json"].as_str()?;
    serde_json::from_str::<Value>(wj).ok()
}

/// The inner data object of a parsed `weight_json` value, whichever serde shape produced it.
///
/// `EdgeWeight` (`crates/schema/src/types/edge.rs`) is `#[serde(tag = "type")]` over struct
/// variants, some of which carry a field literally named `data` -- those land under `"data"`.
/// `NodeWeight` (`crates/schema/src/types/node.rs`) is ALSO `#[serde(tag = "type")]`, but over
/// TUPLE variants (`Property(PropertyData)`, `PoliticalActor(PoliticalActorData)`, ...), so serde
/// flattens the inner struct's fields directly alongside `"type"` with no wrapper key at all.
/// That last case is the fallback: hand back the whole value.
///
/// An earlier revision of this doc claimed `NodeWeight` was adjacently tagged
/// (`#[serde(tag = "type", content = "content")]`) and looked only under `"content"`/`"data"`.
/// It is not, and never was -- so this returned `None` for every real `NodeWeight` row, silently
/// leaving every producer bucket empty. Verified against `node.rs`'s own round-trip unit test,
/// `coordinator.rs::apply_mutation` (which serializes a bare `NodeWeight`), and the CCOD/OCOD
/// adapters' `GraphMutation::AddNode` emit sites.
///
/// Passing the whole value through is safe because every discriminator in `REGISTRY` reads a
/// named data field (`council_name`, `country_incorporated`) that cannot collide with the
/// `"type"` tag key.
fn inner_data(weight: &Value) -> Option<&Value> {
    weight
        .get("content")
        .or_else(|| weight.get("data"))
        .or(Some(weight))
}

/// Evidence attributed to ONE named `source`. If `term`'s registry entry has more than one
/// producer and no discriminator, returns `Err(MultiProducerNoDiscriminator)` -- D1's core
/// refusal guarantee. Otherwise scans `graph_events` for `term`'s type marker and, when a
/// discriminator IS present, additionally filters each row's inner data object through it before
/// counting -- only rows the discriminator accepts count toward `value`.
pub async fn count_for_source(
    db: &Surreal<Any>,
    kind: &str,
    term: &'static str,
    source: &'static str,
) -> Result<Attributed, AttributionError> {
    let entry = lookup(term)?;
    if entry.producers.len() > 1 && entry.discriminator.is_none() {
        return Err(AttributionError::MultiProducerNoDiscriminator {
            term,
            producers: entry.producers,
        });
    }
    let marker = format!("\"type\":\"{term}\"");
    let rows = scan::scan_events_by_type(db, kind, &marker)
        .await
        .map_err(|e| AttributionError::ScanFailed(e.to_string()))?;
    from_rows(term, source, &rows)
}

/// The total evidence count for `term` across every registered producer -- D3's shape. Never
/// refuses on producer count: there is no "wrong" source to attribute to when the caller wants
/// the total across every registered producer. When `splitter` is supplied, rows are additionally
/// bucketed by calling it on each row's inner data object to populate the per-producer breakdown;
/// with no splitter, `producers` is a single `(REGISTRY entry's producers[0], value)` tuple, or
/// empty when `producers` is the empty-slice case (`OffshoreLink`).
pub async fn count_across_producers(
    db: &Surreal<Any>,
    kind: &str,
    term: &'static str,
    splitter: Option<fn(&Value) -> &'static str>,
) -> Result<Attributed, AttributionError> {
    let entry = lookup(term)?;
    let marker = format!("\"type\":\"{term}\"");
    let rows = scan::scan_events_by_type(db, kind, &marker)
        .await
        .map_err(|e| AttributionError::ScanFailed(e.to_string()))?;
    let value = rows.len() as u64;

    let producers = if let Some(splitter) = splitter {
        let mut buckets: HashMap<&'static str, u64> = HashMap::new();
        for row in &rows {
            let weight = weight_of(row);
            if let Some(data) = weight.as_ref().and_then(inner_data) {
                *buckets.entry(splitter(data)).or_insert(0) += 1;
            }
        }
        let mut breakdown: Vec<(&'static str, u64)> = buckets.into_iter().collect();
        // Deterministic order -- a HashMap's iteration order is not, and this feeds directly
        // into report text later phases render.
        breakdown.sort_by_key(|(name, _)| *name);
        breakdown
    } else if let Some(&first) = entry.producers.first() {
        vec![(first, value)]
    } else {
        Vec::new()
    };

    Ok(Attributed {
        value,
        term,
        producers,
    })
}

/// Wraps an already-fetched row set (no second query) into an [`Attributed]` value, applying the
/// SAME refusal rule as [`count_for_source`]. For probes that need the raw rows for non-evidence
/// logic (an intersection with another edge type) AND want to report the row count as attributed
/// evidence without querying twice.
pub fn from_rows(
    term: &'static str,
    source: &'static str,
    rows: &[Value],
) -> Result<Attributed, AttributionError> {
    let entry = lookup(term)?;
    if entry.producers.len() > 1 && entry.discriminator.is_none() {
        return Err(AttributionError::MultiProducerNoDiscriminator {
            term,
            producers: entry.producers,
        });
    }
    let value = if let Some(discriminator) = entry.discriminator {
        rows.iter()
            .filter(|r| {
                let weight = weight_of(r);
                weight
                    .as_ref()
                    .and_then(inner_data)
                    .is_some_and(discriminator)
            })
            .count() as u64
    } else {
        rows.len() as u64
    };
    Ok(Attributed {
        value,
        term,
        producers: vec![(source, value)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn count_for_source_refuses_multi_producer_without_discriminator() {
        // `Property` has two producers (land_registry_ccod, land_registry_ocod) and no
        // discriminator in the registry -- the refusal must fire before any query is issued, so
        // an uninitialised `Surreal::init()` handle is safe here (mirrors this codebase's
        // established convention of never calling `.query()` on one, e.g.
        // `identity_integrity.rs`'s `identity_integrity_zero_planning_contracts_is_data_gap_and_blocks`).
        let db: Surreal<Any> = Surreal::init();
        let result = count_for_source(&db, "NodeAdded", "Property", "land_registry_ccod").await;
        assert!(
            matches!(
                result,
                Err(AttributionError::MultiProducerNoDiscriminator {
                    term: "Property",
                    ..
                })
            ),
            "expected MultiProducerNoDiscriminator refusal, got {result:?}"
        );
    }

    #[tokio::test]
    async fn count_for_source_accepts_single_producer() {
        // `Psc` has exactly one producer (ch_psc) -- the refusal branch is unreachable, so this
        // proves the single-producer path returns `Ok`, not the refusal path. A real DB is
        // required past the refusal check (this codebase's convention: never `.query()` an
        // uninitialised `Surreal::init()` handle), so this is DB-gated like every other live-scan
        // test in this crate.
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: count_for_source_accepts_single_producer (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_attributed",
            "single_producer",
        )
        .await;

        let result = count_for_source(&db, "NodeAdded", "Psc", "ch_psc").await;
        assert!(
            matches!(
                result,
                Ok(Attributed {
                    value: 0,
                    term: "Psc",
                    ..
                })
            ),
            "expected Ok(Attributed {{ value: 0, .. }}) against a zero-row namespace, got {result:?}"
        );
    }

    #[test]
    fn from_rows_unknown_term_is_refused() {
        let result = from_rows("NotARealTerm", "some_source", &[]);
        assert!(matches!(
            result,
            Err(AttributionError::UnknownTerm("NotARealTerm"))
        ));
    }

    #[test]
    fn from_rows_applies_the_political_actor_discriminator() {
        // Two rows: one carrying `council_name` (council_registers-sourced), one without
        // (electoral_commission-sourced). Attributing to `council_registers` must count only the
        // first.
        let rows = vec![
            serde_json::json!({
                "payload": {
                    "weight_json": serde_json::to_string(&serde_json::json!({
                        "type": "PoliticalActor",
                        "content": { "name": "Cllr A", "party": null, "council_name": "Anglesey Council" }
                    })).unwrap()
                }
            }),
            serde_json::json!({
                "payload": {
                    "weight_json": serde_json::to_string(&serde_json::json!({
                        "type": "PoliticalActor",
                        "content": { "name": "MP B", "party": "Labour Party", "council_name": null }
                    })).unwrap()
                }
            }),
        ];
        let result = from_rows("PoliticalActor", "council_registers", &rows)
            .expect("PoliticalActor has a discriminator, so this must not refuse");
        assert_eq!(
            result.value, 1,
            "only the council_name-bearing row should count"
        );
    }
}
