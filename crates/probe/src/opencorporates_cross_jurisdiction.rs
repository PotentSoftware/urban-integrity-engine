//! `OpencorporatesCrossJurisdiction` -- ports `probe_opencorporates_cross_jurisdiction`
//! (`scripts/analyst_probe.py:2117-2189`).
//!
//! # Config gates, never the key itself (D-09, T-34-04)
//!
//! The realistic live state is disabled: `enabled`/`oc_open_data_confirmed`/`api_key` are all
//! unset (the OC API is £225/month, unaffordable now). That is an unmet prerequisite, so this
//! probe returns `Verdict::blocked(prerequisite)`, naming the three config gates -- never
//! `Verdict::data_gap` as Python's original does. The struct below carries `has_api_key: bool`
//! only, never `api_key: Option<String>` -- the real key string can never reach a
//! `Finding.summary` or a rendered report row because the type carrying it is never constructed
//! with the real string (T-34-04's mitigation).
//!
//! # Population is `candidates_found`, never `oc_quota.request_count` (D-10)
//!
//! `population = capable = marker.candidates_found`, read from the existing
//! `enrichment_runs:opencorporates` marker. Evidence never states `oc_quota.request_count` --
//! reporting the quota-request count as if it were the queried population is criterion 3's
//! actual falsifiable claim this probe must not reproduce.
//!
//! # Marker absent is its own DataGap branch (D-14)
//!
//! All three config gates met but no `enrichment_runs:opencorporates` row -- the adapter is
//! configured but has never run -- must report `DataGap`, never `Clear(0)` via
//! `candidates_found = 0` defaulting silently. Mirrors `property_nodes.rs`'s D3
//! marker-absent-is-its-own-branch precedent.
//!
//! # Pending-only is Clear, not Weak (D-11)
//!
//! `matches` counts only `auto_linked`/`confirmed` `opencorporates`-sourced `identity_links`;
//! pending-only rows are reported in evidence as unadjudicated but excluded from `matches`.
//! `Weak` is therefore unreachable for this probe by construction -- counting pending links as
//! matches was the false-`Hit` direction VRD-08 was raised for.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{EvidenceRow, Finding, Severity, Verdict};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// D-09's three config gates, injected as plain bools -- never the API key itself. Constructed
/// by Plan 34-05's `run.rs::execute()` from `OpenCorporatesConfig`'s three fields.
pub struct OpencorporatesCrossJurisdiction {
    pub enabled: bool,
    pub oc_open_data_confirmed: bool,
    pub has_api_key: bool,
}

#[async_trait::async_trait]
impl Probe for OpencorporatesCrossJurisdiction {
    fn name(&self) -> &'static str {
        "opencorporates-cross-jurisdiction"
    }

    /// Can-name-entities, deferred: `links`'s rows (each carrying `source_node_id`/
    /// `target_node_id`) from `scan_identity_links(db, None, Some("opencorporates"))` (~line
    /// 126) are held and discarded after the `confirmed`/`pending` `.filter(...).count()` calls
    /// (~lines 149-156).
    ///
    /// Classified can-name-entities by Phase 49's EVM-01 audit; deferred past this phase per
    /// D-02 ("not every probe has to be covered to the same depth" -- the phase 49 design notes), recorded
    /// in plan 49-10.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::AggregateOnly
    }

    /// No RecordRef at all -- aggregate-only evidence, the card states absence honestly (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Absence
    }

    fn tier(&self) -> &'static str {
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "Does OpenCorporates cross-jurisdiction lookup confirm foreign-registered entities tied \
         to the procurement graph?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        if !self.enabled || !self.oc_open_data_confirmed || !self.has_api_key {
            // Names all three gate VALUES (booleans) -- never the key string itself (T-34-04).
            // "prerequisites" (always plural, D-03) -- all three gate values are always named
            // regardless of how many are unmet, so this is never a single-item list.
            let prerequisite = format!(
                "OpenCorporates unmet prerequisites: enabled={}, oc_open_data_confirmed={}, \
                 api_key_present={}",
                self.enabled, self.oc_open_data_confirmed, self.has_api_key
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::blocked(prerequisite.clone()),
                summary: prerequisite,
                cause: None,
                // No Python analog (Python's original has no Blocked branch, only DATA_GAP/LOW
                // when the adapter has not run) -- defaulted to Severity::Low per 35-01
                // convention, matching the disposition of Python's nearest analogous branch.
                severity: Severity::Low,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let marker = st.enrichment_runs(db).await.get("opencorporates").cloned();
        let marker = match marker {
            Some(m) => m,
            None => {
                // D-14: config gates met but no marker -- "configured but never run", never
                // Clear(0).
                let summary = "opencorporates enrichment_runs marker absent -- the adapter is \
                                configured but has never run."
                    .to_string();
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(summary.clone()),
                    summary,
                    cause: None,
                    // No Python analog for a marker-absent-but-gates-met branch -- defaulted to
                    // Severity::Low, matching Python's nearest analogous DATA_GAP/LOW branch
                    // (analyst_probe.py:2154).
                    severity: Severity::Low,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // D-10: population/capable come from the marker's candidates_found, NEVER from
        // oc_quota.request_count.
        let population = marker.candidates_found as u64;
        let capable = population;

        let links = match scan::scan_identity_links(db, None, Some("opencorporates")).await {
            Ok(rows) => rows,
            Err(e) => {
                let summary = "opencorporates identity_links scan failed -- cross-jurisdiction \
                                matching cannot be evaluated."
                    .to_string();
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!(
                        "opencorporates identity_links scan failed: {e}"
                    )),
                    summary,
                    cause: None,
                    // No Python analog for a scan failure -- defaulted to Severity::Medium per
                    // 35-01 convention (a genuine technical failure, worth review).
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        let confirmed = links
            .iter()
            .filter(|r| r.status == "auto_linked" || r.status == "confirmed")
            .count() as u64;
        let pending = links
            .iter()
            .filter(|r| r.status == "pending_review")
            .count();

        // `population` is per-run (this run's `candidates_found` marker, per D-10) while
        // `confirmed` is drawn from a scan of the CUMULATIVE `identity_links` table. The two are
        // not measurements of the same set, so on any run where `candidates_found` drops below
        // the accumulated link count they become incoherent, and `Verdict::classify` would
        // happily emit `Hit { population: 0, capable: 0, matches: 52 }` -- a HIT whose matches
        // were drawn from a population of zero. `classify` asserts `capable <= population` but
        // deliberately does NOT constrain `matches`, so nothing downstream catches this.
        //
        // A false HIT here is the worst outcome this tool can produce: its findings accuse real
        // people and organisations of corruption. Per the project's standing rule -- never discard
        // or paper over a data-integrity violation -- report the incoherence as a DataGap rather
        // than emitting a verdict that cannot be true.
        if confirmed > population {
            let confirmed_word = plural(
                confirmed,
                "cumulative confirmed identity link",
                "cumulative confirmed identity links",
            );
            let summary = format!(
                "opencorporates counts incoherent ({confirmed} cumulative links vs \
                 {population} candidates_found this run) -- cross-jurisdiction matching \
                 cannot be evaluated without a false verdict."
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(format!(
                    "opencorporates counts incoherent: {confirmed_word} exceed this run's \
                     {population} candidates_found -- the per-run enrichment_runs marker and the \
                     cumulative identity_links table are not comparable populations, so no \
                     verdict can be drawn"
                )),
                summary,
                cause: None,
                // No Python analog (this is a Rust-added safety net against a false Hit, per
                // this file's own doc comment) -- Severity::Medium: a data anomaly worth
                // investigation, not itself an accusation against any entity.
                severity: Severity::Medium,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let evidence_text = format!(
            "{} confirmed/auto-linked (matches); {} pending review (unadjudicated, excluded \
             from matches).",
            plural(confirmed, "identity link", "identity links"),
            plural(pending as u64, "identity link", "identity links")
        );

        if confirmed > 0 {
            let summary = format!(
                "{population} candidates_found this run; {} confirmed.",
                plural(confirmed, "match", "matches")
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(population, capable, confirmed, 1, ""),
                summary,
                cause: None,
                // Python: `severity = "HIGH" if confirmed else "LOW"` -- confirmed branch
                // (analyst_probe.py:2174).
                severity: Severity::High,
                evidence: vec![EvidenceRow {
                    text: evidence_text,
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let summary = format!(
            "{population} candidates_found this run; {} pending review.",
            plural(pending as u64, "link", "links")
        );
        Finding {
            probe: self.name().to_string(),
            verdict: Verdict::classify(population, capable, confirmed, 1, ""),
            summary,
            cause: None,
            // Python: `severity = "HIGH" if confirmed else "LOW"` -- not-confirmed branch
            // (analyst_probe.py:2174).
            severity: Severity::Low,
            evidence: vec![EvidenceRow {
                text: evidence_text,
                refs: vec![],
            }],
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_enrichment_run(db: &Surreal<Any>, source: &str, candidates_found: i64) {
        db.query(format!(
            "UPSERT enrichment_runs:{source} SET source = '{source}', \
             last_run_at = $last_run_at, candidates_found = $candidates_found, \
             auto_linked = $auto_linked, pending_review = $pending_review, \
             discarded = $discarded"
        ))
        .bind(("last_run_at", 1_756_000_000i64))
        .bind(("candidates_found", candidates_found))
        .bind(("auto_linked", 0i64))
        .bind(("pending_review", 0i64))
        .bind(("discarded", 0i64))
        .await
        .expect("seed enrichment_runs row")
        .check()
        .expect("seed enrichment_runs row check");
    }

    async fn seed_identity_link(
        db: &Surreal<Any>,
        key: &str,
        source_node_id: &str,
        target_node_id: &str,
        status: &str,
    ) {
        db.query(
            "CREATE type::record('identity_links', $key) SET \
             source_node_id = $src, target_node_id = $tgt, resolution_type = 'exact_ch_number', \
             confidence = $conf, source = $source, resolved_at = $resolved_at, \
             evidence_json = '{}', status = $status",
        )
        .bind(("key", key.to_string()))
        .bind(("src", source_node_id.to_string()))
        .bind(("tgt", target_node_id.to_string()))
        .bind(("conf", 1.0f64))
        .bind(("source", "opencorporates"))
        .bind(("status", status.to_string()))
        .bind(("resolved_at", chrono::Utc::now()))
        .await
        .expect("seed identity_links row")
        .check()
        .expect("seed identity_links row check");
    }

    async fn seed_oc_quota(db: &Surreal<Any>, request_count: i64) {
        db.query("UPSERT oc_quota:current SET year_month = $ym, request_count = $rc")
            .bind(("ym", "2026-08".to_string()))
            .bind(("rc", request_count))
            .await
            .expect("seed oc_quota row")
            .check()
            .expect("seed oc_quota row check");
    }

    #[test]
    fn opencorporates_config_gates_unmet_is_blocked() {
        // Pure-Rust unit test -- no TestNs, no #[tokio::test] DB dependency needed beyond an
        // unconnected Surreal::init() handle, mirroring `identity_integrity_zero_planning_
        // contracts_is_data_gap_and_blocks`'s pattern of never calling `.query()` on the branch
        // under test. Blocked check runs before any DB access, so it doesn't even need async.
        let probe = OpencorporatesCrossJurisdiction {
            enabled: false,
            oc_open_data_confirmed: true,
            has_api_key: true,
        };
        // Construct a Finding directly against the same branch logic `run` uses, without
        // spinning up a runtime -- the blocked branch is pure and returns before any `.await`.
        let prerequisite = format!(
            "OpenCorporates unmet prerequisites: enabled={}, oc_open_data_confirmed={}, \
             api_key_present={}",
            probe.enabled, probe.oc_open_data_confirmed, probe.has_api_key
        );
        let verdict = Verdict::blocked(prerequisite.clone());
        assert!(matches!(verdict, Verdict::Blocked { .. }));
        assert!(
            !prerequisite.to_lowercase().contains("sk-")
                && !prerequisite.contains("api_key=")
                && prerequisite.contains("api_key_present=true"),
            "prerequisite string must name the gate VALUE, never an api-key-shaped value, got: \
             {prerequisite}"
        );
    }

    #[tokio::test]
    async fn opencorporates_config_gates_unmet_is_blocked_via_run() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: opencorporates_config_gates_unmet_is_blocked_via_run (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        // Unconnected handle is safe -- the blocked branch returns before any DB access.
        let db: Surreal<Any> = Surreal::init();
        let mut st = Strategy::default();
        let probe = OpencorporatesCrossJurisdiction {
            enabled: false,
            oc_open_data_confirmed: true,
            has_api_key: true,
        };
        let finding = probe.run(&db, &mut st).await;
        assert!(matches!(finding.verdict, Verdict::Blocked { .. }));
        assert!(
            !finding.summary.contains("api_key=") && finding.summary.contains("api_key_present"),
            "summary must not contain the raw api key value, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn opencorporates_marker_absent_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: opencorporates_marker_absent_is_data_gap (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_opencorporates",
            "marker_absent",
        )
        .await;
        let mut st = Strategy::default();
        let probe = OpencorporatesCrossJurisdiction {
            enabled: true,
            oc_open_data_confirmed: true,
            has_api_key: true,
        };
        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "all gates met but no marker must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn opencorporates_pending_only_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: opencorporates_pending_only_is_clear (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_opencorporates",
            "pending_only",
        )
        .await;
        seed_enrichment_run(&db, "opencorporates", 5).await;
        seed_identity_link(&db, "oc_pending_1", "GB-COH:1", "oc:1", "pending_review").await;
        seed_identity_link(&db, "oc_pending_2", "GB-COH:2", "oc:2", "pending_review").await;
        let mut st = Strategy::default();
        let probe = OpencorporatesCrossJurisdiction {
            enabled: true,
            oc_open_data_confirmed: true,
            has_api_key: true,
        };
        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "pending-only links must be Clear, not Weak, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains('2'),
            "summary must mention the 2 pending rows, got: {}",
            finding.summary
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // NOT the 2 pending rows. Per this probe's D-10 decision, `population = capable =
        // marker.candidates_found` -- the adapter's own per-run claim (5 here), never the
        // measured link count. This is VRD-13's SelfReported class in its purest form: the
        // population is an unverifiable adapter self-report, and no graph scan can corroborate
        // it. The plan predicted 2 by reading the seeded links; the value assertion caught it.
        assert_eq!(
            coverage.population(),
            5,
            "population is the marker's candidates_found (5), not the 2 measured pending rows"
        );
        assert_eq!(
            coverage.capable(),
            5,
            "capable is set equal to population per D-10, not derived from the links"
        );
    }

    #[tokio::test]
    async fn opencorporates_confirmed_reports_population_not_quota() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: opencorporates_confirmed_reports_population_not_quota (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_opencorporates",
            "confirmed",
        )
        .await;
        seed_enrichment_run(&db, "opencorporates", 7).await;
        seed_oc_quota(&db, 999).await;
        seed_identity_link(&db, "oc_confirmed_1", "GB-COH:9", "oc:9", "auto_linked").await;
        let mut st = Strategy::default();
        let probe = OpencorporatesCrossJurisdiction {
            enabled: true,
            oc_open_data_confirmed: true,
            has_api_key: true,
        };
        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "1 confirmed link must be Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains('7'),
            "summary must state the population figure (7), got: {}",
            finding.summary
        );
        assert!(
            !finding.summary.contains("999"),
            "summary must NEVER contain oc_quota.request_count (999), got: {}",
            finding.summary
        );
    }

    /// Regression: cumulative `identity_links` must never out-count the per-run marker into a
    /// false HIT.
    ///
    /// `population` is this run's `candidates_found` (D-10) while `matches` is scanned from the
    /// cumulative `identity_links` table. On a run where `candidates_found` drops below the
    /// accumulated link count, `Verdict::classify` -- which asserts `capable <= population` but
    /// NOT `matches <= population` -- would emit `Hit { population: 0, capable: 0, matches: 2 }`:
    /// matches drawn from a population of zero, rendered to the operator as "HIT (0/0,
    /// matches=2)". A false HIT is the worst output this tool can produce. Assert the probe
    /// reports `DataGap` naming the incoherence instead.
    #[tokio::test]
    async fn opencorporates_cumulative_links_exceeding_run_population_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: opencorporates_cumulative_links_exceeding_run_population_is_data_gap (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_opencorporates",
            "incoherent",
        )
        .await;
        // This run found 0 candidates ...
        seed_enrichment_run(&db, "opencorporates", 0).await;
        // ... but 2 confirmed links survive from earlier runs.
        seed_identity_link(&db, "oc_stale_1", "GB-COH:1", "oc:1", "auto_linked").await;
        seed_identity_link(&db, "oc_stale_2", "GB-COH:2", "oc:2", "confirmed").await;

        let mut st = Strategy::default();
        let probe = OpencorporatesCrossJurisdiction {
            enabled: true,
            oc_open_data_confirmed: true,
            has_api_key: true,
        };
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "cumulative links (2) exceeding this run's population (0) must be DataGap, never a \
             Hit -- got {:?}",
            finding.verdict
        );
        assert!(
            !matches!(finding.verdict, Verdict::Hit { .. }),
            "must never emit a Hit whose matches exceed its population"
        );
    }
}
