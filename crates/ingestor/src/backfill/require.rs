//! Fail-loud accessors for fields a caller cannot work without.
//!
//! ## The defect these replace
//!
//! `row["timestamp"].as_u64().unwrap_or(0)` reads as defensive. It is not: it converts *"the query
//! did not return this column"* into *"the value is zero"*, and zero is a perfectly plausible
//! number that flows onward and produces a wrong answer nothing logs.
//!
//! This exact line has now cost the project twice:
//!
//! 1. `enricher/src/property_accumulation.rs` — the entity-status tie-break scored every row 0, so
//!    the winning status was whichever row happened to land last.
//! 2. `backfill/cessation.rs` — `event_day` became `1970-01-01` for every row, and the classifier
//!    reported **2** stamped entities where the graph holds **1,966**. It passed all 16 unit tests,
//!    because every fixture supplied a timestamp the real query did not.
//!
//! Both times the fix was to add the column to the `SELECT` — and both times the `unwrap_or(0)`
//! was left in place, still armed for the next query change. That is the bug: the guard absorbs
//! the failure instead of reporting it.
//!
//! A `require_*` call returns `Err` naming the field and the row, so the third occurrence is an
//! error at the moment it happens rather than a plausible number discovered months later.

use anyhow::{Context, Result};

/// A required `u64`. Errors if the field is absent or not a number.
pub fn require_u64(row: &serde_json::Value, field: &str) -> Result<u64> {
    row.get(field)
        .and_then(|v| v.as_u64())
        .with_context(|| missing(row, field, "u64"))
}

/// A required string. Errors if the field is absent or not a string.
pub fn require_str<'a>(row: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(|v| v.as_str())
        .with_context(|| missing(row, field, "string"))
}

/// A required string at `row[outer][inner]` — the shape every `graph_events` payload read takes.
pub fn require_nested_str<'a>(
    row: &'a serde_json::Value,
    outer: &str,
    inner: &str,
) -> Result<&'a str> {
    row.get(outer)
        .and_then(|v| v.get(inner))
        .and_then(|v| v.as_str())
        .with_context(|| missing(row, &format!("{outer}.{inner}"), "string"))
}

/// The error text. Names the field, the expected type, and the keys that *were* present — which is
/// what tells an operator "the SELECT is missing a column" rather than "the data is bad".
fn missing(row: &serde_json::Value, field: &str, expected: &str) -> String {
    let present: Vec<&str> = row
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    format!(
        "required field `{field}` ({expected}) is absent or the wrong type; the row carries \
         {present:?} — if the field is missing entirely, the query's SELECT is the likely cause"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> serde_json::Value {
        serde_json::json!({
            "id": "graph_events:abc",
            "timestamp": 1_785_196_800_000u64,
            "payload": { "entity_id": "GB-COH:99000042" }
        })
    }

    #[test]
    fn a_present_field_is_returned() {
        assert_eq!(require_u64(&row(), "timestamp").unwrap(), 1_785_196_800_000);
        assert_eq!(require_str(&row(), "id").unwrap(), "graph_events:abc");
        assert_eq!(
            require_nested_str(&row(), "payload", "entity_id").unwrap(),
            "GB-COH:99000042"
        );
    }

    /// **The regression this module exists for.** A column the query never selected must be an
    /// error, not a zero. `unwrap_or(0)` returned 0 here and the caller computed `1970-01-01`.
    #[test]
    fn a_missing_column_is_an_error_not_a_plausible_default() {
        let mut r = row();
        r.as_object_mut().unwrap().remove("timestamp");
        let err = require_u64(&r, "timestamp").expect_err("must not succeed");
        let msg = err.to_string();
        assert!(msg.contains("timestamp"), "{msg}");
        assert!(
            msg.contains("SELECT"),
            "the message must point at the likely cause: {msg}"
        );
    }

    /// The error names the keys that were present, so an operator can tell a missing column apart
    /// from bad data without opening the code.
    #[test]
    fn the_error_names_the_fields_that_were_present() {
        let mut r = row();
        r.as_object_mut().unwrap().remove("timestamp");
        let msg = require_u64(&r, "timestamp").unwrap_err().to_string();
        assert!(msg.contains("id"), "{msg}");
        assert!(msg.contains("payload"), "{msg}");
    }

    /// A field present but of the wrong type is just as wrong as an absent one, and `unwrap_or`
    /// hides both identically.
    #[test]
    fn a_wrongly_typed_field_is_also_an_error() {
        let r = serde_json::json!({ "timestamp": "1785196800000" });
        assert!(
            require_u64(&r, "timestamp").is_err(),
            "a stringified number must not silently read as absent-or-zero"
        );
    }

    #[test]
    fn a_missing_nested_field_is_an_error() {
        let r = serde_json::json!({ "payload": { "other": 1 } });
        assert!(require_nested_str(&r, "payload", "entity_id").is_err());
        assert!(require_nested_str(&r, "absent", "entity_id").is_err());
    }
}
