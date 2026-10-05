//! `PrecomputedAlerts` Tier-1 probe — ports `probe_precomputed_alerts`
//! (`scripts/analyst_probe.py:554-580`), closing VRD-06's `precomputed-alerts` hazard IN FULL
//! (phase 32 design decision D1/D1a): a zero `MotifDetected` count is only honestly `Clear` if the motif
//! detector actually ran. Without the `detector_runs:current` marker 32-01 shipped, zero alerts
//! was indistinguishable from "the detector has never run".
//!
//! **That marker gate was REMOVED in Phase 40 (ADJ-02, `docs/ADJUDICATIONS.md`).** This header
//! used to say the probe "checks the marker FIRST, before ever counting `MotifDetected` rows, so
//! that distinction is structural, not incidental". It no longer does, and had not since Phase 40
//! — see the D-01/D-03 comment at the marker read below: execution always falls through to the
//! count-and-classify path regardless of marker state. The marker is written only by
//! `GraphCoordinator::bake_snapshot`, which covers Tier-B motifs alone, while five enricher
//! post-adapter detectors emit `MotifDetected` without ever touching it — so a missing marker was
//! never evidence that no alerts exist. The "did Tier-B run" signal moved to the
//! `detector-readiness` probe as a non-gating evidence row. The marker is still read here, but
//! only so a read *failure* can report `DataGap`.
//!
//! # `payload.motif_type` does not exist — a plan-vs-reality correction
//!
//! This plan's own `<interfaces>` section describes a `MotifDetected` row shape of
//! `payload: { "motif_type": ..., "result_json": ... }`. Every real writer of `MotifDetected`
//! rows in this codebase (`crates/core/src/persistence/surreal.rs:109-111`'s
//! `GraphEventKind::MotifDetected { result_json }` mapping, and every enricher post-adapter
//! detector — `evasion.rs:187`, `donation_correlation.rs:366`, `offshore_shadow.rs:218`,
//! `charity_crossover.rs:270`, `property_accumulation.rs:244` — writes `payload: {
//! "result_json": "<json string>" }` and NOTHING else. There is no top-level
//! `payload.motif_type` field anywhere in production data; `motif_type` lives *inside* the
//! `result_json` string, alongside `risk_score`/`description`/dedup keys
//! (`crates/detector/src/alert/result.rs:14`'s `MotifResult.motif_type`,
//! `evasion.rs:169-176`'s `result_json` literal).
//!
//! This project has hit this exact defect class before: `crates/enricher/src/adapters/
//! opencorporates.rs`'s `alerted_entity_ids` read `payload.entity_id` — a field no alert had
//! ever carried (measured live: 1,571/1,571 alerts had `payload` keys `== {result_json}`) — and
//! silently matched nothing for the adapter's entire life. Reading `payload.motif_type` directly
//! here would reproduce that same defect: the top-8 breakdown would show every real motif as
//! `"unknown"` while the population count `n` stayed correct, silently breaking T-32-03's
//! mitigation (the breakdown "structurally cannot diverge" from the population it's drawn from —
//! a breakdown that is always `{unknown: n}` does not diverge in count, but it is useless).
//!
//! Fix (Rule 1 — auto-fixed bug, not a deviation from the plan's *intent*): parse
//! `payload.result_json` as a JSON string, then read `motif_type` from the parsed value. Falls
//! back to `"unknown"` only if a row's `result_json` is missing/unparseable or lacks
//! `motif_type` — never a page-abandoning error, since a handful of malformed legacy rows must
//! not cost the probe its actionable HIGH verdict on the rest.

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::{detector_runs, scan};
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// How many motif types the evidence breakdown names, worst offender first. Verbatim port of
/// Python's `LIMIT 8` (`scripts/analyst_probe.py:577`).
const BREAKDOWN_TOP_N: usize = 8;

/// Bounded sample size for the per-motif-type ref list — see
/// `disqualified_director_edges.rs`'s identical constant. Load-bearing per T-49-01: this probe is
/// the highest-population priority probe on the live corpus (1,947 `MotifDetected` events,
/// 2026-09-17), so an uncapped ref list per motif type would produce an oversized
/// `/graph?ids=...` URL. Applied per motif type, not per row — the population cannot inflate a
/// single evidence row's ref count past this.
const DISPLAY_CAP: usize = 8;

/// The D1/D1a detector-run-marker consumer — see module doc comment.
pub struct PrecomputedAlerts;

/// `payload.result_json`'s `motif_type`, or `"unknown"` if the row is malformed. See the module
/// doc comment for why this reads *inside* `result_json` rather than a nonexistent
/// `payload.motif_type` field.
fn motif_type_of(row: &serde_json::Value) -> String {
    row["payload"]["result_json"]
        .as_str()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("motif_type")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// The entity ids `payload.result_json` names for `row`, or an empty `Vec` if the row is
/// malformed or its `motif_type` is not one of the six known values.
///
/// The id-bearing field is NOT uniform across `motif_type` (the plan 49-01 summary's Assumption A1,
/// checked against live source): the three Tier-B types (`PhoenixLoop`/`AddressCluster`/
/// `NepotismBridge`) serialize a whole `MotifResult` with a top-level `participants: Vec<String>`
/// array; the five enricher post-adapter types each write their own ad-hoc JSON object with
/// different, motif-type-specific id field name(s) and no `participants` array at all:
///
/// | `motif_type` | id field(s) |
/// |---|---|
/// | `PhoenixLoop`, `AddressCluster`, `NepotismBridge` | `participants` (array of strings) |
/// | `disqualified_director_evasion` | `officer_id`, `disq_node_id` |
/// | `property_accumulation_anomaly` | `entity_id` |
/// | `donation_contract_correlation` | `donor_id` only — `party_name`/`lpa_code` are display text, not ids |
/// | `offshore_shadow` | `entity_id`, `icij_node_id` |
/// | `charity_commercial_crossover` | `officer_id`, `charity_node_id` |
///
/// Mirrors `motif_type_of`'s parse-failure discipline: any failure at any step (missing
/// `result_json` string, unparseable JSON, unrecognised `motif_type`, a non-string
/// `participants` element) yields `vec![]`, never a panic — matches PRB-08 (`Probe::run` is
/// infallible by design).
fn entity_ids_of(row: &serde_json::Value) -> Vec<String> {
    let parsed: Option<serde_json::Value> = row["payload"]["result_json"]
        .as_str()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let Some(v) = parsed else {
        return vec![];
    };
    let motif_type = v.get("motif_type").and_then(|t| t.as_str()).unwrap_or("");

    match motif_type {
        "PhoenixLoop" | "AddressCluster" | "NepotismBridge" => v
            .get("participants")
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        "disqualified_director_evasion" => field_strs(&v, &["officer_id", "disq_node_id"]),
        "property_accumulation_anomaly" => field_strs(&v, &["entity_id"]),
        "donation_contract_correlation" => field_strs(&v, &["donor_id"]),
        "offshore_shadow" => field_strs(&v, &["entity_id", "icij_node_id"]),
        "charity_commercial_crossover" => field_strs(&v, &["officer_id", "charity_node_id"]),
        _ => vec![],
    }
}

/// Reads each of `fields`, in order, from `v` as a string, collecting only the ones present — a
/// row missing one of two id fields still contributes the one it has.
fn field_strs(v: &serde_json::Value, fields: &[&str]) -> Vec<String> {
    fields
        .iter()
        .filter_map(|f| v.get(*f).and_then(|x| x.as_str()).map(str::to_owned))
        .collect()
}

/// Groups `rows` by `motif_type`, sorts descending by count with a name tie-break for
/// determinism (Python's `ORDER BY n DESC` has no explicit tie-break, but a nondeterministic
/// evidence string would make the DB-gated fixtures flaky), and keeps the top
/// [`BREAKDOWN_TOP_N`]. Shared by `top8_breakdown`'s formatting and `entity_ref_rows`'s
/// per-motif-type ref construction, so the two structurally cannot diverge from each other or
/// from the population count `n` — see T-32-03's mitigation in the plan's threat model.
fn top_n_motif_types(rows: &[serde_json::Value]) -> Vec<(String, u64)> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for row in rows {
        *counts.entry(motif_type_of(row)).or_insert(0) += 1;
    }
    let mut counted: Vec<(String, u64)> = counts.into_iter().collect();
    counted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counted.truncate(BREAKDOWN_TOP_N);
    counted
}

/// Formats [`top_n_motif_types`] as Python's `f"{n:>4}  {t}"` (`scripts/analyst_probe.py:578`).
fn top8_breakdown(rows: &[serde_json::Value]) -> String {
    top_n_motif_types(rows)
        .iter()
        .map(|(t, c)| format!("{c:>4}  {t}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One `EvidenceRow` per top-8 motif type that contributes at least one extractable id, each
/// capped at [`DISPLAY_CAP`] `RecordRef`s (T-49-01). Built per motif type rather than per raw
/// row — this keeps the row count bounded the same way [`top8_breakdown`] already is. A motif
/// type in the top-8 that contributes zero ids (e.g. every row of that type failed to parse) is
/// skipped rather than pushing a misleading "0 unique" row.
fn entity_ref_rows(rows: &[serde_json::Value]) -> Vec<EvidenceRow> {
    let mut out = Vec::new();
    for (motif_type, _count) in top_n_motif_types(rows) {
        let mut ids: Vec<String> = rows
            .iter()
            .filter(|r| motif_type_of(r) == motif_type)
            .flat_map(entity_ids_of)
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            continue;
        }
        let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
        out.push(EvidenceRow {
            text: format!(
                "{motif_type}: {} unique named entities, showing up to {DISPLAY_CAP}: {}",
                ids.len(),
                capped
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            refs: capped
                .iter()
                .map(|id| RecordRef {
                    kind: RecordKind::Entity,
                    id: (*id).clone(),
                })
                .collect(),
        });
    }
    out
}

#[async_trait::async_trait]
impl Probe for PrecomputedAlerts {
    fn name(&self) -> &'static str {
        "precomputed-alerts"
    }

    /// Searchable by the findings-by-entity index (PIV-02) as of Phase 49: `result_json` names
    /// participant/subject entity ids per motif type, extracted via `entity_ids_of`; the mapping
    /// is NOT uniform across `motif_type` values (see that function's own doc comment) — do not
    /// assume `participants` exists on every row.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesNodes
    }

    /// A bag of named-entity chips, no dedicated motif renderer (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Chips
    }

    fn tier(&self) -> &'static str {
        "Tier-1"
    }

    fn hypothesis(&self) -> &'static str {
        "Are alerts already computed by the live motif detector present, and non-empty?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // The has-ever-run check is authoritative and happens FIRST, before any MotifDetected
        // scan — T-32-01's mitigation. A scan failure here is a DataGap Finding, not a
        // propagated Result — Probe::run is infallible by design (PRB-08).
        let has_run = match detector_runs::detector_has_ever_run(db).await {
            Ok(has_run) => has_run,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("detector_runs table read failed: {e}")),
                    summary: "Could not read the detector_runs table — precomputed-alerts \
                              capability cannot be evaluated."
                        .to_string(),
                    cause: None,
                    // No Python analog (Python does not check a detector_runs marker at all) —
                    // defaulted to Medium per 35-01's analog-less-DataGap convention.
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // D-01/D-03: no early return on `!has_run` here — a has-never-run state is not, by
        // itself, evidence that no alerts exist. There are two independent producers of
        // `MotifDetected` (the Tier-B motif detector, tracked by `detector_runs`, and five
        // enricher post-adapter detectors that never touch that table), so execution always
        // falls through to the count-and-classify path below regardless of run state. The "did
        // Tier-B actually run" signal is surfaced separately by `detector-readiness`.
        let rows = match scan::scan_events_by_kind(db, "MotifDetected").await {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("MotifDetected scan failed: {e}")),
                    summary: "MotifDetected scan failed — precomputed alerts cannot be counted."
                        .to_string(),
                    cause: None,
                    // No Python analog — defaulted to Medium per 35-01's analog-less-DataGap
                    // convention.
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        let n = rows.len() as u64;

        // population = capable = matches = n; hit_threshold = 1, so ANY non-zero alert count is
        // a Hit — matches Python's binary `n > 0 -> HIT` treatment exactly.
        let verdict = Verdict::classify(n, n, n, 1, "");

        // D-02: split the top-8 breakdown out of `summary` into a typed evidence row — `summary`
        // stays a single sentence sized for a ledger cell.
        let (summary, evidence) = if n > 0 {
            let summary = format!(
                "{} — review via `urban-integrity alerts list`.",
                plural(n, "pre-computed motif alert", "pre-computed motif alerts")
            );
            let mut evidence = vec![EvidenceRow {
                text: top8_breakdown(&rows),
                refs: vec![],
            }];
            evidence.extend(entity_ref_rows(&rows));
            (summary, evidence)
        } else {
            // D-04: the zero-alert summary must not over-claim generically that "the detector
            // has run" — table presence only proves *Tier-B* ran, not the five enricher
            // post-adapter detectors, whose run status this table never tracks either way.
            let summary = if has_run {
                "Zero MotifDetected events (the detector_runs table shows Tier-B has run at \
                 least once; the five enricher post-adapter detectors' run status is not \
                 tracked by this marker). Forensic analysis proceeds from raw nodes/edges."
                    .to_string()
            } else {
                "Zero MotifDetected events. The detector_runs table is empty — Tier-B motif \
                 detector run status unknown, and the five enricher post-adapter detectors' run \
                 status is not tracked at all. Forensic analysis proceeds from raw nodes/edges."
                    .to_string()
            };
            (summary, vec![])
        };

        // Matches Python's binary HIT/HIGH vs CLEAR/INFO split (scripts/analyst_probe.py:567,576).
        let severity = if n > 0 {
            Severity::High
        } else {
            Severity::Info
        };

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motif_row(motif_type: &str) -> serde_json::Value {
        let result_json = serde_json::json!({
            "motif_type": motif_type,
            "risk_score": 0.9,
        })
        .to_string();
        serde_json::json!({ "payload": { "result_json": result_json } })
    }

    #[test]
    fn motif_type_of_reads_inside_result_json_not_a_nonexistent_top_level_field() {
        let row = motif_row("phoenix_loop");
        assert_eq!(motif_type_of(&row), "phoenix_loop");
    }

    #[test]
    fn motif_type_of_falls_back_to_unknown_on_malformed_result_json() {
        let row = serde_json::json!({ "payload": { "result_json": "not json" } });
        assert_eq!(motif_type_of(&row), "unknown");

        let row = serde_json::json!({ "payload": {} });
        assert_eq!(motif_type_of(&row), "unknown");
    }

    #[test]
    fn top8_breakdown_sorts_descending_and_formats_like_python() {
        let rows = vec![
            motif_row("phoenix_loop"),
            motif_row("address_cluster"),
            motif_row("address_cluster"),
            motif_row("address_cluster"),
        ];
        let breakdown = top8_breakdown(&rows);
        assert_eq!(breakdown, "   3  address_cluster\n   1  phoenix_loop");
    }

    #[test]
    fn top8_breakdown_truncates_to_eight_types() {
        let rows: Vec<serde_json::Value> =
            (0..10).map(|i| motif_row(&format!("motif_{i}"))).collect();
        let breakdown = top8_breakdown(&rows);
        assert_eq!(breakdown.lines().count(), 8);
    }

    // ── DB-gated dual-branch fixtures (D1/D1a) ─────────────────────────────────────────────

    async fn seed_motif_event(db: &Surreal<Any>, motif_type: &str, suffix: &str) {
        let result_json = serde_json::json!({
            "motif_type": motif_type,
            "risk_score": 0.9,
            "description": "test alert",
        })
        .to_string();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("MotifDetected-{motif_type}-{suffix}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "MotifDetected",
                    "payload": { "result_json": result_json },
                }),
            ))
            .await
            .expect("failed to seed MotifDetected event")
            .check()
            .expect("failed to seed MotifDetected event (check)");
    }

    /// Like [`seed_motif_event`], but the caller supplies the full `result_json` object —
    /// needed for Task 2's multi-shape fixtures, since `seed_motif_event` only ever writes
    /// `motif_type`/`risk_score`/`description` and never a `participants` array or any
    /// enricher-style id field.
    async fn seed_motif_event_with_result_json(
        db: &Surreal<Any>,
        event_id: &str,
        result_json_value: &serde_json::Value,
    ) {
        let result_json = result_json_value.to_string();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "MotifDetected",
                    "payload": { "result_json": result_json },
                }),
            ))
            .await
            .expect("failed to seed MotifDetected event")
            .check()
            .expect("failed to seed MotifDetected event (check)");
    }

    #[tokio::test]
    async fn precomputed_alerts_no_marker_with_alerts_present_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_no_marker_with_alerts_present_is_hit (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Fixture 1: no detector_runs:current row, and a stray MotifDetected row present anyway
        // — D-01/D-03: marker absence must not suppress a real alert count. One of the five
        // enricher post-adapter detectors could have written this row without ever touching the
        // Tier-B marker.
        seed_motif_event(&db, "phoenix_loop", "stray").await;

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "no detector_runs marker with alerts present must be Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains("phoenix_loop")),
            "evidence must prove the alert was actually counted, got: {:?}",
            finding.evidence
        );
        assert!(
            !finding.summary.to_lowercase().contains("never run"),
            "the 'never run' DataGap-branch text no longer exists for this state, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn precomputed_alerts_marker_present_zero_motifs_is_clear_and_says_detector_ran() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_marker_present_zero_motifs_is_clear_and_says_detector_ran (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Fixture 2: detector_runs seeded via 46-04's history writer, zero MotifDetected rows.
        detector_runs::record_detector_run_history(
            &db,
            detector_runs::DetectorRunRecord {
                run_id: "test-run-0".to_string(),
                started_at_ms: 1_700_000_000_000,
                pipeline: "core".to_string(),
                motif_types: vec![],
                motif_count: 0,
                imd_loaded: None,
                imd_csv_path: None,
                imd_denominator_declared: None,
            },
        )
        .await
        .expect("seed detector_runs history row");

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "run recorded + zero MotifDetected rows must be Clear, got {:?}",
            finding.verdict
        );
        // D-04 narrows this away from the generic "detector has run" phrasing this test
        // originally pinned — the summary now confirms Tier-B *specifically* ran ("has run at
        // least once"), not "the detector" generically (which would over-claim about the five
        // enricher post-adapter detectors too). The falsifiable Clear-vs-DataGap distinction is
        // still proven below via the "never run" negative assertion.
        assert!(
            finding
                .summary
                .to_lowercase()
                .contains("has run at least once"),
            "Clear summary must explicitly confirm Tier-B DID run — the falsifiable distinction \
             from the DataGap branch's 'empty' text — got: {}",
            finding.summary
        );
        assert!(
            !finding.summary.to_lowercase().contains("never run"),
            "Clear summary text must differ observably from the DataGap branch's text, got: {}",
            finding.summary
        );
        // D-04: prove the narrowing — the summary must name Tier-B specifically, not claim
        // generically that "the detector" (i.e. all five enricher post-adapter detectors too)
        // has run. Table presence only proves Tier-B ran.
        assert!(
            finding
                .summary
                .contains("the detector_runs table shows Tier-B has run at least once"),
            "Clear summary must name Tier-B specifically, not over-claim generically, got: {}",
            finding.summary
        );
        // The substring assertions above test a different property (the Clear-vs-DataGap text
        // distinction) and are kept, not replaced: a summary string mentioning a number is not
        // proof the number is in the Coverage struct. Unlike the other four probes in this plan,
        // this one's `n` is genuinely computed (`n = rows.len()`, no zero-guard), so pinning 0
        // proves the computed path really did reach zero.
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- zero MotifDetected rows with the detector marker present"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) when no motif rows exist"
        );
    }

    #[tokio::test]
    async fn precomputed_alerts_marker_present_five_motifs_is_hit_with_breakdown() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_marker_present_five_motifs_is_hit_with_breakdown (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Fixture 3: detector_runs seeded, 5 MotifDetected rows across 2 distinct
        // motif_type values.
        detector_runs::record_detector_run_history(
            &db,
            detector_runs::DetectorRunRecord {
                run_id: "test-run-5".to_string(),
                started_at_ms: 1_700_000_000_000,
                pipeline: "core".to_string(),
                motif_types: vec![],
                motif_count: 5,
                imd_loaded: None,
                imd_csv_path: None,
                imd_denominator_declared: None,
            },
        )
        .await
        .expect("seed detector_runs history row");
        for i in 0..3 {
            seed_motif_event(&db, "phoenix_loop", &format!("hit-{i}")).await;
        }
        for i in 0..2 {
            seed_motif_event(&db, "address_cluster", &format!("hit-{i}")).await;
        }

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "marker present + 5 MotifDetected rows must be Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains('5'),
            "summary must state the population figure, got: {}",
            finding.summary
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains("phoenix_loop") && e.text.contains("address_cluster")),
            "evidence must name both motif_type values in the breakdown, got: {:?}",
            finding.evidence
        );
    }

    #[tokio::test]
    async fn precomputed_alerts_no_marker_zero_motifs_is_clear_without_marker() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_no_marker_zero_motifs_is_clear_without_marker (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Fixture 4: no detector_runs:current row, zero MotifDetected rows — the fourth
        // reachable state (marker absent, n==0), previously unreachable (it always returned
        // DataGap before D-01/D-03).

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "no marker + zero MotifDetected rows must be Clear, not DataGap, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("The detector_runs table is empty"),
            "summary must explicitly say the detector_runs table is empty, got: {}",
            finding.summary
        );
    }

    // ── Task 2: per-motif-type ref extraction across shapes ────────────────────────────────

    #[tokio::test]
    async fn precomputed_alerts_extracts_ids_per_motif_type_shape() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_extracts_ids_per_motif_type_shape (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // One Tier-B row (participants array) and one enricher post-adapter row (named id
        // fields), the two shapes entity_ids_of must both handle correctly.
        seed_motif_event_with_result_json(
            &db,
            "MotifDetected-phoenix-shape",
            &serde_json::json!({
                "motif_type": "PhoenixLoop",
                "risk_score": 0.9,
                "participants": ["director:planted-x", "GB-COH:planted-y", "GB-COH:planted-z"],
            }),
        )
        .await;
        seed_motif_event_with_result_json(
            &db,
            "MotifDetected-evasion-shape",
            &serde_json::json!({
                "motif_type": "disqualified_director_evasion",
                "officer_id": "director:planted-officer",
                "disq_node_id": "director:planted-disq",
            }),
        )
        .await;

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        let phoenix_row = finding
            .evidence
            .iter()
            .find(|e| e.text.starts_with("PhoenixLoop:"))
            .unwrap_or_else(|| {
                panic!(
                    "no PhoenixLoop entity-ref evidence row, got: {:?}",
                    finding.evidence
                )
            });
        assert_eq!(
            phoenix_row.refs.len(),
            3,
            "PhoenixLoop row must carry 3 refs (its participants array), got: {:?}",
            phoenix_row.refs
        );
        assert!(
            phoenix_row
                .refs
                .iter()
                .all(|r| r.kind == RecordKind::Entity),
            "every ref must be RecordKind::Entity, got: {:?}",
            phoenix_row.refs
        );

        let evasion_row = finding
            .evidence
            .iter()
            .find(|e| e.text.starts_with("disqualified_director_evasion:"))
            .unwrap_or_else(|| {
                panic!(
                    "no disqualified_director_evasion entity-ref evidence row, got: {:?}",
                    finding.evidence
                )
            });
        assert_eq!(
            evasion_row.refs.len(),
            2,
            "evasion row must carry 2 refs (officer_id + disq_node_id), got: {:?}",
            evasion_row.refs
        );
        assert!(
            evasion_row
                .refs
                .iter()
                .all(|r| r.kind == RecordKind::Entity),
            "every ref must be RecordKind::Entity, got: {:?}",
            evasion_row.refs
        );
    }

    #[tokio::test]
    async fn precomputed_alerts_caps_refs_at_display_cap_per_motif_type() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_caps_refs_at_display_cap_per_motif_type (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // 9 distinct PhoenixLoop rows, each with a single unique participant id — the combined
        // ref count for this motif type must cap at DISPLAY_CAP (8), not 9.
        for i in 0..9 {
            seed_motif_event_with_result_json(
                &db,
                &format!("MotifDetected-phoenix-cap-{i}"),
                &serde_json::json!({
                    "motif_type": "PhoenixLoop",
                    "participants": [format!("director:planted-cap-{i}")],
                }),
            )
            .await;
        }

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        let phoenix_row = finding
            .evidence
            .iter()
            .find(|e| e.text.starts_with("PhoenixLoop:"))
            .unwrap_or_else(|| {
                panic!(
                    "no PhoenixLoop entity-ref evidence row, got: {:?}",
                    finding.evidence
                )
            });
        assert_eq!(
            phoenix_row.refs.len(),
            8,
            "9 unique ids across 9 PhoenixLoop rows must cap at DISPLAY_CAP=8, got: {:?}",
            phoenix_row.refs
        );
        assert!(
            phoenix_row.text.contains("9 unique"),
            "the row's text must still state the true total (9), even though refs are capped at \
             8, got: {}",
            phoenix_row.text
        );
    }

    #[tokio::test]
    async fn precomputed_alerts_unknown_motif_type_contributes_no_refs_without_panicking() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: precomputed_alerts_unknown_motif_type_contributes_no_refs_without_panicking \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // A motif_type not in the known-six list, with no participants/id fields at all —
        // entity_ids_of must return vec![] for it, never panic, and the row must still count
        // toward the population n.
        seed_motif_event_with_result_json(
            &db,
            "MotifDetected-future-unknown",
            &serde_json::json!({
                "motif_type": "future_motif_type",
                "risk_score": 0.5,
            }),
        )
        .await;

        let probe = PrecomputedAlerts;
        let finding = probe.run(&db, &mut st).await;

        let (coverage, matches) = match &finding.verdict {
            Verdict::Hit { coverage, matches } => (coverage, matches),
            other => panic!("expected Hit (the row still counts toward n), got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            1,
            "the unrecognised-motif-type row must still count toward the population"
        );
        assert_eq!(*matches, 1);

        assert!(
            !finding
                .evidence
                .iter()
                .any(|e| e.refs.iter().any(|r| r.id.contains("future_motif_type"))),
            "an unrecognised motif_type must never name itself as a ref, got: {:?}",
            finding.evidence
        );
        // No entity-ref row is emitted for a motif type that contributed zero ids.
        assert!(
            !finding
                .evidence
                .iter()
                .any(|e| e.text.starts_with("future_motif_type:")),
            "a motif type contributing zero ids must not get a misleading '0 unique' row, got: \
             {:?}",
            finding.evidence
        );
    }
}
