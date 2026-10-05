//! The population oracle's independent counting path (ORC-05/ORC-06).
//!
//! # This module must never call `persistence::scan.rs`
//!
//! `crates/core/src/persistence/scan.rs` is the codebase's load-bearing read path for
//! `graph_events` — nearly every enricher adapter, every probe, and most `cli` commands route
//! through it. That is precisely why a defect in `scan.rs` needs an *independent* second
//! implementation to be visible at all: `scan.rs::latest_entity_weights`'s own doc comment
//! states the rule this module exists to satisfy — "the population oracle must not route through
//! `persistence::scan`, so that a defect in one implementation is visible as a disagreement with
//! the other rather than silently shared."
//!
//! This is not a hypothetical concern. On 2026-08-27 `crates/cli/src/commands/export_graphml.rs`
//! independently reproduced the correct Contract population (39,860, ward-reachable) while three
//! probes — reading through `scan.rs::scan_entities_by_type`, which counts one row per *event*,
//! not per entity — reported 99,599. Nothing compared the two figures for four days (quick task
//! `260831-ffx`). A shared implementation would have been wrong in both places and agreed with
//! itself; this module exists so that never happens again for the counting path itself.
//!
//! Every function below is therefore built on raw `db.query()` / `count()` / `GROUP BY` SurrealQL
//! only. This file contains **zero** imports from, or calls into,
//! `urban_integrity_core::persistence::scan` — enforced structurally (not just by convention) by
//! `crates/core/tests/oracle_independence_guard.rs`.
//!
//! # Query shapes here are NOT uniformly safe — one family is proven, two queries are exempted
//!
//! An earlier version of this comment claimed the whole module was immune to the Rule-1
//! planner-truncation bug because it is built from `count()`/`GROUP BY` and issues "never a
//! row-returning `SELECT`". **That claim was false and has been withdrawn** (CR-01,
//! the phase 38 code review). The accurate position, function by function:
//!
//! - [`count_events`] is genuinely inside the proven-safe family. It is a bare server-side
//!   aggregate over the predicate: `SELECT count() FROM graph_events WHERE … GROUP ALL`. No rows
//!   are materialised, which is exactly the measured property behind the exemption — `count()` on
//!   a predicate stays correct while the row-returning form over the *same* predicate truncates.
//!
//! - [`count_distinct_entities`] and [`count_distinct_edge_field`] are **not**. Each wraps an
//!   inner, row-returning `SELECT` over `graph_events` that pairs the indexed `kind` equality
//!   with an unindexed `payload.weight_json CONTAINS` — verbatim Rule 1. The outer aggregate does
//!   not make the inner row set stop being materialised. `query_shape_guard.rs` originally could
//!   not see this, because `broken_rule` short-circuited on the substring `count(` before
//!   evaluating any rule and the outer aggregate supplied it. That exemption has since been
//!   narrowed to exclude nested selects, so both queries are now flagged, and both carry an
//!   explicit `ALLOWLIST` entry recording the evidence for accepting them.
//!
//! That evidence is a live cross-check against a third, unrelated query shape
//! (`array::len(array::distinct(…))`) on `an_anglesey_aebaa785`, 2026-09-01, agreeing exactly at
//! 39,860 and 61,756. **It is a sample, not a proof** — the planner bug is run-to-run
//! non-deterministic, so agreeing observations cannot establish immunity. Re-run that check
//! before trusting this oracle on a new corpus, a new SurrealDB version, or a materially larger
//! graph.
//!
//! Why this matters more than it looks: an under-counting oracle presents as `declared > oracle`,
//! which reads as "the probe over-counts". That is precisely the signal ADJ-01 produced, and the
//! recorded response (`c8fb0c7`) was to change the probe. A truncated oracle could therefore
//! drive a correct probe into being "fixed" to match it.
//!
//! # Scope: `land-flip-candidates` is deliberately excluded, for a reason that is NOT "it routes
//! through `persistence::scan`"
//!
//! `land-flip-candidates` (probe #22) is excluded from this oracle's scope. The population it
//! reports (`land_flip_candidates.rs:110`) is **arithmetic on two scalar fields of a single
//! `land_flip_runs:current` marker record** (`applications_scanned - gate1_no_ppd_match`), not a
//! count of any set — there is nothing here to `GROUP BY`. The marker read
//! (`land_flip_runs::get_land_flip_run_marker`) does not touch `persistence::scan` at all, so the
//! probe is not excluded because of an ORC-06 conflict. It is excluded because the only
//! independent check available — comparing `count()` on `land_flip_gate1_survivors` against the
//! marker's own scalars — is **already enforced inside the probe itself**
//! (`land_flip_candidates.rs:159`: `survivors.len() as u64 != population` → `DataGap`). An oracle
//! re-reading the same two scalars a second time would agree by construction and detect nothing
//! new. Keeping this module to a single query-shape family (`count()`/`GROUP BY` over
//! `graph_events`) rather than growing a second native-table counting path keeps both the ORC-06
//! source guard and the ORC-07 fixture scoped to event staging only.

use anyhow::Context as _;
use serde_json::json;
use surrealdb::{engine::any::Any, Surreal};

/// Extract the `count` field from a `GROUP ALL` response, distinguishing the two states
/// `unwrap_or(0)` used to conflate: an EMPTY match set (SurrealDB returns no row at all — a
/// legitimate 0) versus a row that came back WITHOUT an integer `count` field (the response
/// shape changed under a SurrealDB bump — an error, never a 0). The ICIJ adapter (log claimed
/// 814k written, table empty) and the PPD ingest (2,795,151 "written", 0 staged) both shipped
/// on exactly this conflation; oracle counts feed verdict disagreement tables, where a silent
/// 0 fabricates evidence.
fn extract_count(rows: &[serde_json::Value], caller: &str) -> anyhow::Result<u64> {
    match rows.first() {
        None => Ok(0), // GROUP ALL over an empty match set yields no row: genuine 0
        Some(r) => {
            let n = r["count"].as_i64().ok_or_else(|| {
                anyhow::anyhow!(
                    "oracle::{caller}: response row carried no integer `count` field \
                     (SurrealDB response shape changed?): {r}"
                )
            })?;
            Ok(n.max(0) as u64)
        }
    }
}

/// Count of `graph_events` rows matching `kind`, optionally ANDed with one or more
/// `payload.weight_json CONTAINS` marker predicates.
///
/// `markers` is empty for a bare kind-only count (e.g. `count_events(db, "MotifDetected", &[])`).
/// Every marker is bound individually as `$m{i}` — never interpolated into the SQL string itself
/// — so a marker value can never widen or narrow the query shape at runtime.
pub async fn count_events(db: &Surreal<Any>, kind: &str, markers: &[&str]) -> anyhow::Result<u64> {
    let mut sql = String::from("SELECT count() FROM graph_events WHERE kind = $kind");
    for i in 0..markers.len() {
        sql.push_str(&format!(" AND payload.weight_json CONTAINS $m{i}"));
    }
    sql.push_str(" GROUP ALL");

    let mut q = db.query(sql).bind(("kind", kind.to_string()));
    for (i, marker) in markers.iter().enumerate() {
        q = q.bind((format!("m{i}"), json!(marker)));
    }
    let rows: Vec<serde_json::Value> = q
        .await
        .context("oracle::count_events query failed")?
        .take(0)?;
    extract_count(&rows, "count_events")
}

/// Count of DISTINCT `payload.entity_id` values among `graph_events` rows matching `kind` and
/// `type_marker`, optionally restricted to entity ids starting with `entity_id_prefix`.
///
/// This is the dedup-by-entity primitive: `GROUP BY entity_id` before the outer `count()` means a
/// re-ingested entity (multiple `NodeAdded` rows for the same `entity_id`) contributes exactly
/// once, the same guarantee `persistence::scan`'s `scan_latest_entities_by_type` provides via its
/// "greatest timestamp wins" Rust-side dedup — reached here by an entirely different mechanism
/// (server-side `GROUP BY`, no row materialisation, no timestamp comparison at all).
/// `crates/core/tests/oracle_shapes.rs` cross-checks this function's count against that other
/// function's own `.len()` on a controlled fixture as the correctness proof for this shape
/// (Pitfall 2 in the phase 38 research notes).
///
/// Row gate (WR-02): `scan_latest_entities_by_type` drops a row when `payload.entity_id` is
/// absent, empty, or non-string (see `entity_id_of`, `scan.rs`), and when `weight_json` fails to
/// parse. Without a matching gate here, each such row forms its own `GROUP BY` bucket and
/// produces `oracle == declared + 1` — a structural false disagreement indistinguishable in the
/// comparison table from a real defect. The `type::is_string(...) AND != ''` predicates below
/// replicate the two cheap cases server-side; `type::is_string` also excludes a
/// RecordId-coerced id (an object), which `scan` would have normalised to `"tb:id"` — so a
/// coerced id shows up as an oracle UNDERCOUNT (a visible disagreement), never as a phantom
/// extra group. The one remaining divergence — a row whose `weight_json` contains the marker
/// substring but is not valid JSON — is not gated here (no cheap server-side JSON-validity
/// test); it is documented rather than silently claimed away. Evidence scope: the cross-check in
/// `oracle_shapes.rs` seeds dirty rows (empty and RecordId-shaped ids) and asserts oracle ==
/// scan; the earlier claim that production Contract/Location ids are "never RecordId-coerced"
/// rested only on test-fixture seed helpers and is withdrawn.
pub async fn count_distinct_entities(
    db: &Surreal<Any>,
    kind: &str,
    type_marker: &str,
    entity_id_prefix: Option<&str>,
) -> anyhow::Result<u64> {
    let mut sql = String::from(
        "SELECT count() FROM (SELECT payload.entity_id AS entity_id FROM graph_events \
         WHERE kind = $kind AND payload.weight_json CONTAINS $marker \
         AND type::is_string(payload.entity_id) AND payload.entity_id != ''",
    );
    if entity_id_prefix.is_some() {
        sql.push_str(" AND string::starts_with(payload.entity_id, $prefix)");
    }
    sql.push_str(" GROUP BY entity_id) GROUP ALL");

    let mut q = db
        .query(sql)
        .bind(("kind", kind.to_string()))
        .bind(("marker", json!(type_marker)));
    if let Some(prefix) = entity_id_prefix {
        q = q.bind(("prefix", prefix.to_string()));
    }
    let rows: Vec<serde_json::Value> = q
        .await
        .context("oracle::count_distinct_entities query failed")?
        .take(0)?;
    extract_count(&rows, "count_distinct_entities")
}

/// Which side of an `EdgeAdded` event's payload [`count_distinct_edge_field`] counts distinct
/// values of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeField {
    /// `payload.src_id`.
    Src,
    /// `payload.dst_id`.
    Dst,
}

/// Count of DISTINCT `payload.src_id` or `payload.dst_id` values among `EdgeAdded` rows matching
/// `type_marker`.
///
/// Always scoped to `kind = 'EdgeAdded'` — this function is inherently edge-only, so there is no
/// `kind` parameter to get wrong. `field` resolves to a compile-time-chosen static field path
/// (`payload.src_id`/`payload.dst_id`), safe to `format!()` directly into the SQL string because
/// it is never derived from external input — only `type_marker` crosses that boundary, and it is
/// always `.bind()`-parameterised.
pub async fn count_distinct_edge_field(
    db: &Surreal<Any>,
    type_marker: &str,
    field: EdgeField,
) -> anyhow::Result<u64> {
    let field_path = match field {
        EdgeField::Src => "payload.src_id",
        EdgeField::Dst => "payload.dst_id",
    };
    let sql = format!(
        "SELECT count() FROM (SELECT {field_path} AS v FROM graph_events \
         WHERE kind = 'EdgeAdded' AND payload.weight_json CONTAINS $marker \
         GROUP BY v) GROUP ALL"
    );
    let rows: Vec<serde_json::Value> = db
        .query(sql)
        .bind(("marker", json!(type_marker)))
        .await
        .context("oracle::count_distinct_edge_field query failed")?
        .take(0)?;
    extract_count(&rows, "count_distinct_edge_field")
}

/// Extract the `max_ts` field from a `GROUP ALL` `math::max(timestamp)` response, mirroring
/// [`extract_count`]'s discipline but for a differently-shaped empty case: unlike `count()`
/// (which yields NO row at all over an empty match set), SurrealDB's `math::max` over `GROUP ALL`
/// yields exactly one row with `max_ts: null` when there is nothing to aggregate — verified
/// against the live instance (a `count()`-shaped "no row" assumption here failed loudly with
/// `{"max_ts":null}` the first time this ran DB-gated, corrected to this JSON-null-aware form).
/// Both "no row at all" and "one row with `max_ts: null`" are therefore genuine `None` (no
/// matching `graph_events` row exists for this prefix at all — legitimate, e.g. a source that
/// has never ingested). A row present with the `max_ts` KEY ABSENT ENTIRELY, or present with a
/// non-null, non-integer value, is a hard error naming the caller (the response shape changed
/// under a SurrealDB bump).
fn extract_max_ts(rows: &[serde_json::Value], caller: &str) -> anyhow::Result<Option<u64>> {
    match rows.first() {
        None => Ok(None), // No row at all — genuinely no matching events.
        Some(r) => match r.get("max_ts") {
            None => Err(anyhow::anyhow!(
                "oracle::{caller}: response row carried no `max_ts` key at all \
                 (SurrealDB response shape changed?): {r}"
            )),
            Some(v) if v.is_null() => Ok(None), // math::max over an empty GROUP ALL: genuine None.
            Some(v) => {
                let n = v.as_i64().ok_or_else(|| {
                    anyhow::anyhow!(
                        "oracle::{caller}: response row's `max_ts` field was neither null nor \
                         an integer (SurrealDB response shape changed?): {r}"
                    )
                })?;
                Ok(Some(n.max(0) as u64))
            }
        },
    }
}

/// The maximum `timestamp` among `graph_events` rows matching `kind` whose `payload.entity_id`
/// starts with `entity_id_prefix` — the last-event-per-source primitive `crates/workbench::
/// last_event::compute` builds its Home page Ingestors card from.
///
/// `kind`/`entity_id_prefix` are both bound as parameters, never interpolated into the SQL
/// string itself — mirroring [`count_events`]'s already-accepted single-level-aggregate shape
/// (a bare `count()`/`math::max()` over a predicate, no row-returning `SELECT`, no
/// `query_shape_guard.rs` exemption needed).
pub async fn max_timestamp_for_prefix(
    db: &Surreal<Any>,
    kind: &str,
    entity_id_prefix: &str,
) -> anyhow::Result<Option<u64>> {
    let sql = "SELECT math::max(timestamp) AS max_ts FROM graph_events \
               WHERE kind = $kind AND string::starts_with(payload.entity_id, $prefix) \
               GROUP ALL";

    let rows: Vec<serde_json::Value> = db
        .query(sql)
        .bind(("kind", kind.to_string()))
        .bind(("prefix", entity_id_prefix.to_string()))
        .await
        .context("oracle::max_timestamp_for_prefix query failed")?
        .take(0)?;
    extract_max_ts(&rows, "max_timestamp_for_prefix")
}

#[cfg(test)]
mod tests {
    use super::{extract_count, extract_max_ts};

    /// The two states `unwrap_or(0)` used to conflate, now provably distinct (WR-01).
    #[test]
    fn extract_count_distinguishes_empty_set_from_shape_change() {
        // Empty match set: GROUP ALL yields no row at all. Genuine 0.
        assert_eq!(extract_count(&[], "test").unwrap(), 0);

        // Normal row.
        let ok = serde_json::json!({ "count": 7 });
        assert_eq!(extract_count(&[ok], "test").unwrap(), 7);

        // A row came back but carries no integer `count` — the response shape changed.
        // Pre-fix this returned Ok(0), indistinguishable from the empty set; it must error.
        let renamed = serde_json::json!({ "total": 7 });
        let err = extract_count(&[renamed], "test").unwrap_err();
        assert!(
            err.to_string().contains("response shape changed"),
            "shape-change error must say so, got: {err}"
        );

        // Non-integer count is the same failure class.
        let stringly = serde_json::json!({ "count": "7" });
        assert!(extract_count(&[stringly], "test").is_err());
    }

    /// [`extract_max_ts`] mirrors [`extract_count`]'s discipline for the differently-shaped
    /// empty case: BOTH "no row at all" and "one row with `max_ts: null`" (SurrealDB's actual
    /// live-verified `math::max` over an empty `GROUP ALL`) are genuine `None`; a row present
    /// with the `max_ts` key entirely absent, or present with a non-null non-integer value, is a
    /// hard error.
    #[test]
    fn extract_max_ts_distinguishes_empty_set_from_shape_change() {
        // No row at all.
        assert_eq!(extract_max_ts(&[], "test").unwrap(), None);

        // The ACTUAL empty-aggregate shape SurrealDB returns: one row, `max_ts: null`.
        let null_row = serde_json::json!({ "max_ts": null });
        assert_eq!(extract_max_ts(&[null_row], "test").unwrap(), None);

        let ok = serde_json::json!({ "max_ts": 1_700_000_000_000i64 });
        assert_eq!(
            extract_max_ts(&[ok], "test").unwrap(),
            Some(1_700_000_000_000)
        );

        // The `max_ts` key is entirely absent — the response shape changed.
        let renamed = serde_json::json!({ "max": 1_700_000_000_000i64 });
        let err = extract_max_ts(&[renamed], "test").unwrap_err();
        assert!(
            err.to_string().contains("no `max_ts` key at all"),
            "missing-key error must say so, got: {err}"
        );

        // Non-null, non-integer `max_ts` is the same failure class.
        let stringly = serde_json::json!({ "max_ts": "1700000000000" });
        assert!(extract_max_ts(&[stringly], "test").is_err());
    }
}
