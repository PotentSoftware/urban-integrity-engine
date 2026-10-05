//! `_catalog` registry access — the source of truth for what analyses exist.
//!
//! All rows live in the `analyses` table of the `_catalog` database under the
//! configured namespace. `created_at` is an RFC3339 string (SCHEMALESS table),
//! never a coerced `datetime` (see CATALOG_SCHEMA docs).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::config::AppConfig;
use urban_integrity_schema::db::CATALOG_SCHEMA;

/// One row of the `analyses` registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRow {
    pub db_name: String,
    pub label: String,
    pub ward_codes: Vec<String>,
    pub wardset_hash: String,
    #[serde(default)]
    pub baseline_source: Option<String>,
    #[serde(default)]
    pub baseline_vintage: Option<String>,
    #[serde(default)]
    pub boundary_vintage: Option<String>,
    pub schema_version: i64,
    pub checkpoint_path: String,
    pub status: String,
    pub created_at: String, // RFC3339
    #[serde(default)]
    pub retired_at: Option<String>, // RFC3339, set on remove
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub coverage_baseline_json: Option<String>,
    /// `true` only for analyses provisioned with `analysis create --fixture` — the `fixture seed`
    /// subcommand refuses any other target (spec §4.1), so a typo can never point the seeder at a
    /// live corpus.
    #[serde(default)]
    pub fixture: bool,
}

/// Connect to the `_catalog` database and ensure the `analyses` table exists.
pub async fn connect_catalog(config: &AppConfig) -> Result<Surreal<Any>> {
    let http_url = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");
    let db: Surreal<Any> = surrealdb::engine::any::connect(http_url.as_str())
        .await
        .with_context(|| format!("catalog connect failed: {http_url}"))?;
    if let (Some(user), Some(pass)) = (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) {
        db.signin(surrealdb::opt::auth::Root {
            username: user.to_string(),
            password: pass.to_string(),
        })
        .await
        .context("catalog sign-in failed")?;
    }
    db.use_ns(&config.surreal.namespace)
        .use_db("_catalog")
        .await
        .context("catalog namespace/db selection failed")?;
    db.query(CATALOG_SCHEMA)
        .await
        .context("catalog schema init failed")?;
    Ok(db)
}

/// Insert a new registry row. Uses `db_name` as the record id, so a second
/// insert for the same ward set fails — the idempotency guard.
pub async fn insert_analysis(db: &Surreal<Any>, row: &AnalysisRow) -> Result<()> {
    db.query(
        "CREATE type::record('analyses', $db_name) SET \
         db_name = $db_name, label = $label, ward_codes = $ward_codes, \
         wardset_hash = $wardset_hash, baseline_source = $baseline_source, \
         baseline_vintage = $baseline_vintage, boundary_vintage = $boundary_vintage, \
         schema_version = $schema_version, checkpoint_path = $checkpoint_path, \
         status = $status, created_at = $created_at, notes = $notes, fixture = $fixture",
    )
    .bind(("db_name", row.db_name.clone()))
    .bind(("label", row.label.clone()))
    .bind(("ward_codes", row.ward_codes.clone()))
    .bind(("wardset_hash", row.wardset_hash.clone()))
    .bind(("baseline_source", row.baseline_source.clone()))
    .bind(("baseline_vintage", row.baseline_vintage.clone()))
    .bind(("boundary_vintage", row.boundary_vintage.clone()))
    .bind(("schema_version", row.schema_version))
    .bind(("checkpoint_path", row.checkpoint_path.clone()))
    .bind(("status", row.status.clone()))
    .bind(("created_at", row.created_at.clone()))
    .bind(("notes", row.notes.clone()))
    .bind(("fixture", row.fixture))
    .await
    .context("registry insert failed")?
    .check()
    .context("registry insert rejected (duplicate db_name?)")?;
    Ok(())
}

/// Fetch one row by `db_name`, or `None` if absent.
pub async fn get_analysis(db: &Surreal<Any>, db_name: &str) -> Result<Option<AnalysisRow>> {
    let mut res = db
        .query("SELECT * FROM analyses WHERE db_name = $db_name LIMIT 1")
        .bind(("db_name", db_name.to_string()))
        .await
        .context("registry lookup failed")?;
    // The beta SDK's `.take()` only decodes into `serde_json::Value`; convert
    // each row into `AnalysisRow` (extra fields like the `id` Thing are ignored).
    let raw: Vec<serde_json::Value> = res.take(0).context("registry row fetch failed")?;
    match raw.into_iter().next() {
        Some(v) => Ok(Some(
            serde_json::from_value(v).context("registry row decode failed")?,
        )),
        None => Ok(None),
    }
}

/// All rows, newest first (RFC3339 `created_at` sorts lexically).
pub async fn list_analyses(db: &Surreal<Any>) -> Result<Vec<AnalysisRow>> {
    let mut res = db
        .query("SELECT * FROM analyses ORDER BY created_at DESC")
        .await
        .context("registry list failed")?;
    let raw: Vec<serde_json::Value> = res.take(0).context("registry list fetch failed")?;
    raw.into_iter()
        .map(|v| serde_json::from_value(v).context("registry list decode failed"))
        .collect()
}

/// Flip a row's `status` (e.g. `provisioning` → `active`).
pub async fn set_status(db: &Surreal<Any>, db_name: &str, status: &str) -> Result<()> {
    db.query("UPDATE type::record('analyses', $db_name) SET status = $status")
        .bind(("db_name", db_name.to_string()))
        .bind(("status", status.to_string()))
        .await
        .context("registry status update failed")?
        .check()
        .context("registry status update rejected")?;
    Ok(())
}

/// Mark a row `retired` and stamp `retired_at` (RFC3339). Used by `analysis remove`.
pub async fn mark_retired(db: &Surreal<Any>, db_name: &str, retired_at: &str) -> Result<()> {
    db.query("UPDATE type::record('analyses', $db_name) SET status = 'retired', retired_at = $retired_at")
        .bind(("db_name", db_name.to_string()))
        .bind(("retired_at", retired_at.to_string()))
        .await
        .context("registry mark_retired failed")?
        .check()
        .context("registry mark_retired rejected")?;
    Ok(())
}

/// Write the D-05 coverage-regression baseline (a JSON string) for one analysis row.
/// The canonical writer for CLI-driven writes; `scripts/analyst_probe.py` has its own
/// Python-side writer so the probe can self-establish the first baseline.
pub async fn set_coverage_baseline(
    db: &Surreal<Any>,
    db_name: &str,
    coverage_baseline_json: &str,
) -> Result<()> {
    db.query("UPDATE type::record('analyses', $db_name) SET coverage_baseline_json = $coverage_baseline_json")
        .bind(("db_name", db_name.to_string()))
        .bind(("coverage_baseline_json", coverage_baseline_json.to_string()))
        .await
        .context("registry coverage-baseline update failed")?
        .check()
        .context("registry coverage-baseline update rejected")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_row_fixture_defaults_false_and_round_trips() {
        let json = serde_json::json!({
            "db_name": "an_x_deadbeef", "label": "x", "ward_codes": ["X05999001"],
            "wardset_hash": "00", "schema_version": 1, "checkpoint_path": "p",
            "status": "active", "created_at": "2026-01-01T00:00:00Z"
        });
        let row: AnalysisRow = serde_json::from_value(json).unwrap();
        assert!(!row.fixture);
        let mut row = row;
        row.fixture = true;
        let back: AnalysisRow =
            serde_json::from_value(serde_json::to_value(&row).unwrap()).unwrap();
        assert!(back.fixture);
    }
}
