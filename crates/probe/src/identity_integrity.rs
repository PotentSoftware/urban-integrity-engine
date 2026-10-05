//! `IdentityIntegrity` Tier-1 probe — ports `probe_identity_integrity`
//! (`scripts/analyst_probe.py:676-733`).
//!
//! This is the only Tier-1 probe needing a second SurrealDB connection: the D-05
//! coverage-regression baseline lives in `_catalog.analyses.coverage_baseline_json`, a
//! different database from the analysis graph `Probe::run` reads. Per D5 (the phase 32 design notes,
//! locked), that connection is wired as a struct field (`catalog`), never a second parameter on
//! `Probe::run` — the CLI entry point (32-06) constructs it once via `registry::connect_catalog`
//! and hands it to this probe alongside `db_name`, the SAME `resolve_analysis_db(&config)` value
//! the primary connection uses (T-32-01's falsifiable mitigation: no second, independently-read
//! config value that could target the wrong analysis's baseline row).
//!
//! Also closes the `Strategy.identity_blocked` write side of the cross-probe dependency
//! `address-cluster-exact` (32-03's other probe) reads — this probe runs earlier in
//! `GRAPH_PROBES` dispatch order (the plan 32-02 summary's registration list, position 2 of 7).

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::node::ApplicantProvenance;
use urban_integrity_schema::types::verdict::{EvidenceRow, Finding, Severity, Verdict};

use crate::registry;

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// How many percentage points a re-measurement may drop below the stored baseline before this
/// probe treats it as a real regression rather than noise. Cites `analyst_probe.py:587`.
const REGRESSION_TOLERANCE_PTS: f64 = 1.0;

/// The four buckets this probe always reports, in display order. Mirrors
/// `analyst_probe.py:593`'s `_PROVENANCE_BUCKETS`. `"not_yet_measured"` is distinct from
/// `"absent_at_source"`: it means no `applicant_provenance` value has been written at all
/// (pre-D-02 rows), not that the portal was confirmed to have no applicant.
const PROVENANCE_BUCKETS: [&str; 4] = [
    "published",
    "absent_at_source",
    "fetch_failed",
    "not_yet_measured",
];

/// `ApplicantProvenance` bucketed per the exact 4-bucket mapping `analyst_probe.py:690`'s
/// `c.get("applicant_provenance") or "not_yet_measured"` performs — but matched on the typed
/// `Option<ApplicantProvenance>` directly rather than flattened to a bare string early (32-03-PLAN
/// interfaces note, the design notes' Claude's-discretion guidance).
fn bucket_of(provenance: Option<ApplicantProvenance>) -> &'static str {
    match provenance {
        None => "not_yet_measured",
        Some(ApplicantProvenance::Published) => "published",
        Some(ApplicantProvenance::AbsentAtSource) => "absent_at_source",
        Some(ApplicantProvenance::FetchFailed) => "fetch_failed",
    }
}

/// D5 (locked): the only Tier-1 probe needing a second SurrealDB connection.
pub struct IdentityIntegrity {
    /// A `_catalog`-connected handle (`registry::connect_catalog`), distinct from the `db`
    /// argument `run` receives (the current analysis database).
    pub catalog: Surreal<Any>,
    /// The analysis `db_name` this run's `catalog` reads/writes target — threaded from the SAME
    /// `resolve_analysis_db(&config)` call the primary connection uses (T-32-01), never a
    /// second, independently-read config value.
    pub db_name: String,
}

#[async_trait::async_trait]
impl Probe for IdentityIntegrity {
    fn name(&self) -> &'static str {
        "identity-integrity"
    }

    /// **Not** searchable by the findings-by-entity index (PIV-02): counts and a baseline comparison; no id is ever interpolated.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    ///
    /// CONTESTED by Phase 49's EVM-01 re-audit (plan 49-10, 2026-09-17): `run()`'s
    /// `for (_eid, weight) in &planning` loop (~line 156) holds each planning Contract's
    /// entity_id and discards it while bucketing `provenance_counts` -- the same discard shape
    /// as Task 1's five can-name-but-deferred probes (e.g. `geographic-concentration`'s
    /// `for (_entity_id, weight) in &locs`). `population`/`capable` here ARE a count of exactly
    /// those Contract entities, not a fixed structural constant (contrast `detector-readiness`'s
    /// `n_terms`). Left `AggregateOnly` unchanged per this plan's "report the contradiction,
    /// never silently reclassify" instruction -- classification is unresolved pending a future
    /// priority task; see the plan 49-10 summary.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::AggregateOnly
    }

    /// No RecordRef at all -- aggregate-only evidence, the card states absence honestly (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Absence
    }

    fn tier(&self) -> &'static str {
        "Tier-1"
    }

    fn hypothesis(&self) -> &'static str {
        "Is applicant-provenance coverage adequate across planning Contracts, and is the \
         _catalog coverage baseline reachable?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        // `scan_latest_entities_by_type`, NOT `scan_entities_by_type` — the latter returns one
        // entry per `graph_events` row, and `graph_events` is append-only, so a re-ingested
        // Contract appears once per ingest. Counting events as contracts is what produced this
        // probe's false `Critical` "coverage REGRESSED 38.0% vs 95.0%": 37,856 published divided
        // by 99,599 planning EVENTS instead of the 39,860 distinct planning Contracts the
        // `_catalog` baseline was itself recorded over. Last-wins also matters here beyond the
        // count — the provenance value is written by a later pass, so the earlier, provenance-less
        // events for the same Contract must not shadow it.
        let contracts = match scan::scan_latest_entities_by_type(
            db,
            "NodeAdded",
            "\"type\":\"Contract\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result —
                // Probe::run is infallible by design (PRB-08). No Python analog for this
                // branch (Python's fetch_nodes cannot fail this way) -- defaulted to
                // Severity::Medium per 35-01 convention.
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Contract scan failed: {e}")),
                    summary: "Contract scan failed — identity integrity cannot be evaluated."
                        .to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // Restrict to PLANNING Contracts only (entity_id prefix "planning:") — CF contracts
        // carry no applicant_provenance at all and are out of scope for this probe
        // (`analyst_probe.py:677-678`).
        let planning: Vec<&(String, serde_json::Value)> = contracts
            .iter()
            .filter(|(eid, _)| eid.starts_with("planning:"))
            .collect();
        let total = planning.len() as u64;

        if total == 0 {
            // TWO effects in one branch — the Strategy write AND the DataGap — matching
            // Python's `st.identity_blocked = True` immediately before this return.
            st.identity_blocked = true;
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("no planning Contract nodes present"),
                summary:
                    "No planning Contract nodes present — identity-based detection cannot run."
                        .to_string(),
                cause: None,
                // DATA_GAP/HIGH at analyst_probe.py:685 -- the first half of D-01's own
                // same-probe-same-verdict-different-severity justification (see the CRITICAL
                // branch below for the paired case).
                severity: Severity::High,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let mut provenance_counts: HashMap<&'static str, u64> = HashMap::new();
        for (_eid, weight) in &planning {
            let provenance: Option<ApplicantProvenance> = match weight.get("applicant_provenance") {
                None => None,
                Some(v) => serde_json::from_value(v.clone()).ok(),
            };
            *provenance_counts.entry(bucket_of(provenance)).or_insert(0) += 1;
        }
        let published_count = provenance_counts.get("published").copied().unwrap_or(0);
        let pct_published = 100.0 * (published_count as f64) / (total as f64);
        // D-02: the per-bucket breakdown is evidence, not summary prose -- one EvidenceRow per
        // bucket, mirroring Python's own `breakdown_ev` list (`analyst_probe.py:693`).
        let breakdown_evidence: Vec<EvidenceRow> = PROVENANCE_BUCKETS
            .iter()
            .map(|b| EvidenceRow {
                text: format!("{:>6}  {b}", provenance_counts.get(b).copied().unwrap_or(0)),
                refs: vec![],
            })
            .collect();

        // Degrade-safe, matching `read_coverage_baseline`'s convention (`analyst_probe.py:628-655`):
        // any catalog query error, missing row, or malformed JSON is treated as "no baseline",
        // never propagated out of this infallible `run`.
        let baseline_json = registry::coverage_baseline_json(&self.catalog, &self.db_name)
            .await
            .ok()
            .flatten();
        let baseline: Option<(f64, String)> = baseline_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|v| {
                let pct = v.get("pct_published")?.as_f64()?;
                let recorded_at = v
                    .get("recorded_at")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                Some((pct, recorded_at))
            });

        if let Some((baseline_pct, recorded_at)) = &baseline {
            if pct_published < baseline_pct - REGRESSION_TOLERANCE_PTS {
                // The SECOND place this probe sets `identity_blocked` (the first is the
                // zero-Contract branch above).
                st.identity_blocked = true;
                let summary = format!(
                    "Applicant-provenance coverage REGRESSED: {pct_published:.1}% published now \
                     vs {baseline_pct:.1}% at baseline (recorded {recorded_at})."
                );
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(summary.clone()),
                    summary,
                    cause: None,
                    // DATA_GAP/CRITICAL at analyst_probe.py:702 -- the second half of D-01's own
                    // same-probe-same-verdict-different-severity justification (paired with the
                    // HIGH branch above): same probe, same DataGap verdict, different severity.
                    severity: Severity::Critical,
                    evidence: breakdown_evidence,
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        }

        // Ratchet up on improvement (or self-establish a first baseline); never silently
        // ratchet down once a regression check against a real baseline has already passed
        // above (T-32-02's falsifiable mitigation — this branch structure makes a
        // post-regression overwrite impossible by construction, since the regression branch
        // already returned).
        let should_write_baseline = match &baseline {
            None => true,
            Some((baseline_pct, _)) => pct_published > *baseline_pct,
        };
        if should_write_baseline {
            let payload = serde_json::json!({
                "pct_published": pct_published,
                "total": total,
                "recorded_at": chrono::Utc::now().to_rfc3339(),
                "breakdown": provenance_counts,
            })
            .to_string();
            if let Err(e) =
                registry::set_coverage_baseline(&self.catalog, &self.db_name, &payload).await
            {
                tracing::warn!(
                    error = %e,
                    db_name = %self.db_name,
                    "IdentityIntegrity: failed to write coverage baseline (non-fatal)"
                );
            }
        }

        // `matches: 0` always on this path — Python never returns HIT/WEAK for this probe
        // (`analyst_probe.py:725-733` has no such branch). `hit_threshold: u64::MAX` mirrors
        // GeographicConcentration/TemporalBurst's "never Hit/Weak on this path" convention.
        let verdict = Verdict::classify(total, total, 0, u64::MAX, "");
        let summary = format!(
            "Applicant-provenance coverage: {pct_published:.1}% published of {}.",
            plural(total, "planning Contract", "planning Contracts")
        );
        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            // CLEAR/INFO at analyst_probe.py:728 -- Python's own vocabulary for this branch.
            severity: Severity::Info,
            evidence: breakdown_evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::node::{ContractData, NodeWeight};

    async fn seed_contract_event(
        db: &Surreal<Any>,
        entity_id: &str,
        provenance: Option<ApplicantProvenance>,
    ) {
        let weight = NodeWeight::Contract(ContractData {
            name: format!("Contract {entity_id}"),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: provenance,
            portal_detail_handle: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("NodeAdded-{entity_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed contract event");
    }

    /// A SECOND `Surreal<Any>` connection pointed at the same namespace as a `TestNs` handle
    /// but `.use_db("_catalog")`, with `CATALOG_SCHEMA` applied — the composition the research notes'
    /// Test Fixtures section specifies, since no existing helper opens this shape (the phase 32 pattern notes
    /// "No Analog Found").
    async fn connect_test_catalog(ns: &str) -> Surreal<Any> {
        let db: Surreal<Any> = Surreal::init();
        db.connect(urban_integrity_test_support::endpoint())
            .await
            .expect("catalog connect");
        let (username, password) = urban_integrity_test_support::credentials();
        db.signin(surrealdb::opt::auth::Root { username, password })
            .await
            .expect("catalog signin");
        db.use_ns(ns)
            .use_db("_catalog")
            .await
            .expect("catalog use ns/db");
        db.query(urban_integrity_schema::db::CATALOG_SCHEMA)
            .await
            .expect("catalog schema init")
            .check()
            .expect("catalog schema check");
        db
    }

    /// Seeds a minimal `analyses` row via raw SurrealQL — this crate's local `registry` module
    /// deliberately exposes only `coverage_baseline_json`/`set_coverage_baseline` (the two
    /// functions `IdentityIntegrity` itself needs), not the full row-insert API
    /// `crates/cli/src/registry.rs` has; that richer API lives in a different crate this one must
    /// not depend on (that dependency is exactly what the extraction removes).
    async fn seed_analysis_row(catalog: &Surreal<Any>, db_name: &str) {
        catalog
            .query(
                "CREATE type::record('analyses', $db_name) SET \
                 db_name = $db_name, label = 'test fixture', ward_codes = [], \
                 wardset_hash = 'test-hash', schema_version = 1, checkpoint_path = '', \
                 status = 'active', created_at = $created_at",
            )
            .bind(("db_name", db_name.to_string()))
            .bind(("created_at", chrono::Utc::now().to_rfc3339()))
            .await
            .expect("insert analysis row")
            .check()
            .expect("insert analysis row rejected");
    }

    #[tokio::test]
    async fn identity_integrity_zero_planning_contracts_is_data_gap_and_blocks() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_integrity_zero_planning_contracts_is_data_gap_and_blocks (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        // A CF contract (no "planning:" prefix) must NOT count toward `total` — proves the
        // entity_id-prefix filter, not merely "zero Contracts of any kind".
        seed_contract_event(
            &db,
            "GB-CF:not-planning",
            Some(ApplicantProvenance::Published),
        )
        .await;
        let mut st = Strategy::default();

        // The zero-planning-Contracts branch returns before any `catalog` access, so an
        // unconnected handle is safe here — mirrors `mod.rs`'s `StubProbeA`/`StubProbeB`
        // pattern of never calling `.query()` on an uninitialised `Surreal::init()`.
        let catalog: Surreal<Any> = Surreal::init();
        let probe = IdentityIntegrity {
            catalog,
            db_name: "unused".to_string(),
        };
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero planning Contract nodes must be DataGap, got {:?}",
            finding.verdict
        );
        assert!(
            st.identity_blocked,
            "Strategy.identity_blocked must be set on the zero-Contract branch"
        );
    }

    #[tokio::test]
    async fn identity_integrity_catalog_baseline_round_trip_not_mocked() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_integrity_catalog_baseline_round_trip_not_mocked (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let db_name = "test-identity-integrity-c2";

        // Second `_catalog`-scoped connection against the SAME namespace as `db` — the D5
        // second-connection architecture, proven against a real handle (ROADMAP success
        // criterion 2, verbatim: "not a mocked value").
        let catalog = connect_test_catalog(db.ns()).await;
        seed_analysis_row(&catalog, db_name).await;

        // Seed a KNOWN baseline via the EXISTING writer (`registry::set_coverage_baseline`,
        // the canonical CLI-driven writer) and prove `registry::coverage_baseline_json` — the
        // reader this probe calls internally — returns EXACTLY that payload before the probe
        // ever runs.
        let seeded_payload = serde_json::json!({
            "pct_published": 10.0,
            "total": 4,
            "recorded_at": "2020-01-01T00:00:00+00:00",
            "breakdown": {"published": 0, "absent_at_source": 4, "fetch_failed": 0, "not_yet_measured": 0},
        })
        .to_string();
        registry::set_coverage_baseline(&catalog, db_name, &seeded_payload)
            .await
            .expect("seed coverage baseline");

        let read_back = registry::coverage_baseline_json(&catalog, db_name)
            .await
            .expect("coverage_baseline_json");
        assert_eq!(
            read_back.as_deref(),
            Some(seeded_payload.as_str()),
            "registry::coverage_baseline_json must return exactly what set_coverage_baseline \
             wrote — not a mocked value"
        );

        // Planning Contracts whose measured pct_published (50%) exceeds the seeded baseline
        // (10%) by more than the regression tolerance — no regression, so the probe's
        // write-new-baseline branch fires and exercises the read-decide-write cycle end to end.
        seed_contract_event(&db, "planning:c2-1", Some(ApplicantProvenance::Published)).await;
        seed_contract_event(&db, "planning:c2-2", Some(ApplicantProvenance::Published)).await;
        seed_contract_event(
            &db,
            "planning:c2-3",
            Some(ApplicantProvenance::AbsentAtSource),
        )
        .await;
        seed_contract_event(&db, "planning:c2-4", Some(ApplicantProvenance::FetchFailed)).await;

        let mut st = Strategy::default();
        let probe = IdentityIntegrity {
            catalog,
            db_name: db_name.to_string(),
        };
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "50% published vs a 10% baseline is an improvement, not a regression — got {:?}",
            finding.verdict
        );
        assert!(!st.identity_blocked);
        assert!(finding.summary.contains("50.0"));
        assert!(finding.summary.contains('4'));
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // The 4 seeded planning:-prefixed Contract events (c2-1..c2-4). This is the VRD-13
        // canary probe, so its own population figure being right matters more than most.
        assert_eq!(
            coverage.population(),
            4,
            "population must count all 4 seeded planning-prefixed Contract events"
        );
        assert_eq!(
            coverage.capable(),
            4,
            "capable must equal population -- all 4 events are catalog-readable"
        );

        // Final round trip: a FRESH catalog connection reads back the row the probe just
        // wrote, proving the whole read-decide-write cycle used the real `_catalog` database
        // end to end, not an in-memory stand-in.
        let catalog_verify = connect_test_catalog(db.ns()).await;
        let after = registry::coverage_baseline_json(&catalog_verify, db_name)
            .await
            .expect("coverage_baseline_json after run")
            .expect("row still exists");
        let after_json: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(after_json["pct_published"].as_f64(), Some(50.0));
        assert_eq!(after_json["total"].as_u64(), Some(4));
    }

    #[tokio::test]
    async fn identity_integrity_regression_blocks_and_preserves_baseline() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_integrity_regression_blocks_and_preserves_baseline (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let db_name = "test-identity-integrity-c3";

        let catalog = connect_test_catalog(db.ns()).await;
        seed_analysis_row(&catalog, db_name).await;

        // Pre-seed a baseline 20 points above what this run's fixture data will measure (25%).
        let seeded_payload = serde_json::json!({
            "pct_published": 45.0,
            "total": 4,
            "recorded_at": "2020-01-01T00:00:00+00:00",
            "breakdown": {"published": 2, "absent_at_source": 2, "fetch_failed": 0, "not_yet_measured": 0},
        })
        .to_string();
        registry::set_coverage_baseline(&catalog, db_name, &seeded_payload)
            .await
            .expect("seed coverage baseline");

        // 1 of 4 published => 25.0%, more than REGRESSION_TOLERANCE_PTS below the 45% baseline.
        seed_contract_event(&db, "planning:c3-1", Some(ApplicantProvenance::Published)).await;
        seed_contract_event(
            &db,
            "planning:c3-2",
            Some(ApplicantProvenance::AbsentAtSource),
        )
        .await;
        seed_contract_event(
            &db,
            "planning:c3-3",
            Some(ApplicantProvenance::AbsentAtSource),
        )
        .await;
        seed_contract_event(
            &db,
            "planning:c3-4",
            Some(ApplicantProvenance::AbsentAtSource),
        )
        .await;

        let mut st = Strategy::default();
        let probe = IdentityIntegrity {
            catalog,
            db_name: db_name.to_string(),
        };
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "a 25% vs 45% baseline regression must be DataGap, got {:?}",
            finding.verdict
        );
        assert!(
            st.identity_blocked,
            "Strategy.identity_blocked must be set on the regression branch"
        );
        assert!(finding.summary.contains("25.0"));
        assert!(finding.summary.contains("45.0"));

        // T-32-02's belt-and-braces check: the regression branch must NEVER overwrite the
        // pre-seeded (higher) baseline — the branch structure makes this impossible by
        // construction (the regression branch returns before reaching the write), but this
        // proves it observably rather than relying on the structure alone.
        let catalog_verify = connect_test_catalog(db.ns()).await;
        let after = registry::coverage_baseline_json(&catalog_verify, db_name)
            .await
            .expect("coverage_baseline_json after run")
            .expect("row still exists");
        assert_eq!(
            after.as_str(),
            seeded_payload.as_str(),
            "a regression must never ratchet the stored baseline down"
        );
    }
}
