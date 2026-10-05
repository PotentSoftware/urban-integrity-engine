//! Persisting [`DataAnomaly`] rows, and counting what was skipped.
//!
//! A skip that is only logged is a skip nobody will ever find. Every drop that reaches this sink
//! becomes a queryable row and a number in the run's summary.

use crate::backfill::session::DbSession;
use anyhow::Result;
use serde_json::json;
use urban_integrity_schema::anomaly::DataAnomaly;

/// Write one anomaly, keyed deterministically so a re-run overwrites rather than accumulates.
///
/// The write is verified to return a row: SurrealDB reports `status: OK` with an empty result for
/// an UPSERT that matched nothing, and this project has twice reported large phantom write counts
/// against tables that stayed empty.
pub async fn record_anomaly(session: &DbSession, anomaly: &DataAnomaly) -> Result<()> {
    // All-`SET`, with `detected_at` from SurrealQL's own `time::now()`.
    //
    // The obvious `CONTENT $r` form with an RFC3339 string fails: SurrealDB 3.x will not coerce a
    // string into a `datetime` field, and says so — "Expected `datetime` but found
    // '2026-08-18T15:01:29+00:00'". That is the same coercion rule that silently dropped 2.8m PPD
    // rows once, and the only reason it surfaced here instead of vanishing is that this function
    // verifies its write returned a row.
    let written: Vec<serde_json::Value> = session
        .query(
            "UPSERT type::record($tb, $key) SET \
                 kind = $kind, severity = $severity, subject = $subject, detail = $detail, \
                 evidence_json = $evidence, source = $source, status = $status, \
                 detected_at = time::now()",
            &[
                ("tb", json!("data_anomalies")),
                ("key", json!(anomaly.record_key())),
                ("kind", json!(anomaly.kind)),
                ("severity", json!(anomaly.severity.as_str())),
                ("subject", json!(anomaly.subject)),
                ("detail", json!(anomaly.detail)),
                ("evidence", json!(anomaly.evidence_json)),
                ("source", json!(anomaly.source)),
                ("status", json!(anomaly.status.as_str())),
            ],
        )
        .await?
        .take(0)?;
    if written.is_empty() {
        anyhow::bail!(
            "record_anomaly wrote no row for {} — the anomaly itself was lost, which is the \
             failure this table exists to prevent",
            anomaly.record_key()
        );
    }
    Ok(())
}

/// Counts rows a job had to drop, so "skipped" is never invisible.
///
/// Bounded on purpose: a systematically broken query would otherwise write one anomaly per row and
/// bury the finding under its own volume. The first [`Self::SAMPLE_LIMIT`] are recorded in full and
/// the rest are counted, which is enough to diagnose the cause and know the scale.
#[derive(Debug, Default)]
pub struct DropCounter {
    pub dropped: usize,
    recorded: usize,
}

impl DropCounter {
    /// How many individual anomalies to persist before switching to counting only.
    pub const SAMPLE_LIMIT: usize = 20;

    pub fn should_record(&mut self) -> bool {
        self.dropped += 1;
        if self.recorded < Self::SAMPLE_LIMIT {
            self.recorded += 1;
            return true;
        }
        false
    }

    pub fn recorded(&self) -> usize {
        self.recorded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A broken query must not write one anomaly per row — the finding would drown in its own
    /// volume. Record a diagnosable sample, count the rest.
    #[test]
    fn recording_is_sampled_but_counting_is_not() {
        let mut c = DropCounter::default();
        for _ in 0..1000 {
            c.should_record();
        }
        assert_eq!(c.dropped, 1000, "every drop must be counted");
        assert_eq!(
            c.recorded(),
            DropCounter::SAMPLE_LIMIT,
            "only a bounded sample is persisted"
        );
    }

    #[test]
    fn the_first_drops_are_always_recorded() {
        let mut c = DropCounter::default();
        assert!(c.should_record(), "the first drop must be recorded in full");
        assert_eq!(c.dropped, 1);
    }
}
