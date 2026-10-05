//! Data anomalies — a durable destination for input the pipeline could not safely resolve.
//!
//! This project's characteristic failure is **a wrong answer that logs nothing**. The same shape
//! has now been found at least five times: `unwrap_or(0)` absorbing a column the query never
//! selected (twice — `property_accumulation`'s status tie-break and the cessation classifier, which
//! reported 2 stamped entities where the graph held 1,966), a `json!` literal silently dropping
//! `application_received_date` from 30,836 events, `let _ =` discarding every SurrealDB write error
//! in the ICIJ adapter for 814k rows, and a datetime coercion failing silently on 2.8m PPD rows.
//!
//! Every one was found by accident, months later, because the only record was a log line nobody
//! kept. Logs are not a data structure: they are not queried, not retained, and not reported.
//!
//! An anomaly is a **row**. It survives the process, it is queryable, and it appears in the
//! evidence report, so the next occurrence is found by looking rather than by luck.
//!
//! ## An anomaly is not an alert
//!
//! An alert says *"this looks like corruption"*. An anomaly says *"this input could not be
//! trusted, and here is exactly what was skipped"*. Conflating them would put data-quality noise
//! into an analyst's corruption queue, and would let a genuine finding hide among parse failures.

use serde::{Deserialize, Serialize};

/// How much a given anomaly should worry the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnomalySeverity {
    /// Something was skipped, and the skip is expected in normal operation.
    Low,
    /// Something was skipped that the operator would probably want to act on.
    Medium,
    /// Data was withheld to avoid asserting something false, and a human must adjudicate.
    High,
}

impl AnomalySeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Where an anomaly stands with the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnomalyStatus {
    /// Recorded, not yet looked at.
    Open,
    /// A human has looked and accepted the pipeline's decision.
    Reviewed,
    /// A human has looked and judged it not worth acting on.
    Dismissed,
}

impl AnomalyStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Reviewed => "reviewed",
            Self::Dismissed => "dismissed",
        }
    }
}

/// One thing the pipeline refused to guess at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataAnomaly {
    /// Stable machine key for the class of problem, e.g. `officer_key_collision`.
    pub kind: String,
    pub severity: AnomalySeverity,
    /// What it is about — an entity id, a key, a record id. Part of the identity of the anomaly.
    pub subject: String,
    /// One sentence an operator can act on, without reading the code.
    pub detail: String,
    /// Structured supporting facts, as JSON.
    pub evidence_json: String,
    /// The component that raised it, e.g. `backfill:officers`.
    pub source: String,
    pub status: AnomalyStatus,
}

/// Two provably distinct Companies House officers would land on one `director:` key.
pub const KIND_OFFICER_KEY_COLLISION: &str = "officer_key_collision";
/// A row was dropped because a field the caller requires was absent from the query result.
pub const KIND_MISSING_REQUIRED_FIELD: &str = "missing_required_field";

impl DataAnomaly {
    /// The deterministic record key. Re-running a job overwrites its own anomalies rather than
    /// accumulating a fresh row per pass — the defect Open Item 6d found in alert persistence,
    /// where a non-deterministic id produced 17,549 rows for 1,374 distinct clusters.
    pub fn record_key(&self) -> String {
        format!("{}_{}_{}", self.kind, self.source, self.subject)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DataAnomaly {
        DataAnomaly {
            kind: KIND_OFFICER_KEY_COLLISION.to_string(),
            severity: AnomalySeverity::High,
            subject: "director:sample-alex-b197205".to_string(),
            detail: "two Companies House officers share this name and birth month".to_string(),
            evidence_json: r#"{"officer_ids":["a","b"]}"#.to_string(),
            source: "backfill:officers".to_string(),
            status: AnomalyStatus::Open,
        }
    }

    /// Re-running a backfill must overwrite its own anomalies, not stack a new row per pass.
    #[test]
    fn the_record_key_is_deterministic_for_the_same_finding() {
        let a = sample();
        let b = sample();
        assert_eq!(a.record_key(), b.record_key());
        assert_eq!(
            a.record_key(),
            "officer_key_collision_backfill:officers_director:sample-alex-b197205"
        );
    }

    /// Two different subjects under one kind are two different anomalies, or a second collision
    /// would silently overwrite the first — the exact class of loss this table exists to stop.
    #[test]
    fn different_subjects_do_not_collide() {
        let a = sample();
        let mut b = sample();
        b.subject = "director:jones-david".to_string();
        assert_ne!(a.record_key(), b.record_key());
    }

    /// The same subject found by two different components is two findings, not one.
    #[test]
    fn different_sources_do_not_collide() {
        let a = sample();
        let mut b = sample();
        b.source = "backfill:ch".to_string();
        assert_ne!(a.record_key(), b.record_key());
    }

    /// The DB column is a plain string; the enum must round-trip through it unchanged.
    #[test]
    fn severity_and_status_serialise_as_the_strings_the_schema_stores() {
        assert_eq!(AnomalySeverity::High.as_str(), "high");
        assert_eq!(AnomalyStatus::Open.as_str(), "open");
        let json = serde_json::to_string(&sample()).expect("serialise");
        assert!(json.contains(r#""severity":"high""#), "{json}");
        assert!(json.contains(r#""status":"open""#), "{json}");
        let back: DataAnomaly = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back, sample());
    }
}
