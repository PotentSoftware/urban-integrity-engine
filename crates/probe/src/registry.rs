//! `_catalog` registry access — local to this crate, NOT `crates/cli/src/registry.rs`.
//!
//! Mirrors `crates/workbench/src/boot.rs`'s "copied, not called" precedent: `crates/cli`'s
//! `registry.rs` cannot be depended on here (that would create the very `probe -> cli` edge this
//! extraction exists to avoid), so this module reproduces exactly the two functions
//! `identity_integrity.rs` needs, against the identical SurrealQL.
//!
//! [`coverage_baseline_json`] returns only the single field `identity_integrity.rs` reads
//! (`coverage_baseline_json`), not the whole `AnalysisRow` — `crates/cli`'s richer registry (row
//! insert/list/status/retire) has no analog here because nothing in this crate needs it.

use surrealdb::{engine::any::Any, Surreal};

/// Read one analysis row's `coverage_baseline_json` field, or `None` if the row is absent, the
/// field is unset, or the query fails. Degrades safely on any of those — matching
/// `identity_integrity.rs`'s existing `.ok().flatten()` call-site convention, so a catalog outage
/// or missing row is read as "no baseline" rather than propagated as an error.
pub async fn coverage_baseline_json(
    db: &Surreal<Any>,
    db_name: &str,
) -> anyhow::Result<Option<String>> {
    let mut res = db
        .query("SELECT coverage_baseline_json FROM type::record('analyses', $db_name)")
        .bind(("db_name", db_name.to_string()))
        .await?
        .check()?;
    let raw: Vec<serde_json::Value> = res.take(0)?;
    Ok(raw
        .into_iter()
        .next()
        .and_then(|v| v.get("coverage_baseline_json").cloned())
        .and_then(|v| v.as_str().map(str::to_string)))
}

/// Write the D-05 coverage-regression baseline (a JSON string) for one analysis row. Byte-identical
/// SurrealQL to `crates/cli/src/registry.rs::set_coverage_baseline`.
pub async fn set_coverage_baseline(
    db: &Surreal<Any>,
    db_name: &str,
    coverage_baseline_json: &str,
) -> anyhow::Result<()> {
    db.query("UPDATE type::record('analyses', $db_name) SET coverage_baseline_json = $coverage_baseline_json")
        .bind(("db_name", db_name.to_string()))
        .bind(("coverage_baseline_json", coverage_baseline_json.to_string()))
        .await?
        .check()?;
    Ok(())
}
