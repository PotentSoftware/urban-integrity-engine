//! `LandFlipCandidates` — ports `probe_land_flip_candidates` (`scripts/analyst_probe.py:2409-2505`),
//! the milestone's namesake defect (VRD-05).
//!
//! Python's original returns a bare `CLEAR` whenever zero candidate applications were found,
//! regardless of what the underlying gate-1/gate-2/gate-3 population looked like — collapsing
//! "148 gate-1 survivors, 140 of them structurally incapable of ever passing gate-3" into the
//! same verdict as "the detector never ran" or "PPD is empty". This port closes that: population
//! and capability are read exclusively through Plan 34-01's persistence functions
//! (`land_flip_runs`, `land_flip_capability`) — never by re-deriving gate-1/gate-2/gate-3 or
//! importing `crates/enricher` (PRB-03) — and reports `NotApplicable`/`DataGap`/`Hit` split by
//! structural gate-3 capability (D-01..D-08, D-17).
//!
//! # Branch structure (D-01..D-08, D-17)
//!
//! 1. `get_land_flip_run_marker` errors, or returns `None` (or a marker whose `finished_at_ms`
//!    could not be read) → `DataGap` before any population/capability computation is attempted
//!    (D-04).
//! 2. `population == 0` (`applications_scanned - gate1_no_ppd_match`, clamped at 0): distinguish
//!    "PPD never staged" from "PPD staged but nothing matched" via `ppd_transactions_populated`
//!    — D-08's three `DataGap` reasons.
//! 3. `population > 0`: read every gate-1 survivor row. If the row count disagrees with the
//!    marker's `population`, return `DataGap` naming the mismatch BEFORE any capability check —
//!    the defense-in-depth regression test for 34-03's T-34-08 crash window (D-17), never a
//!    fabricated `NotApplicable` computed against a survivors table that cannot be trusted.
//! 4. Otherwise evaluate `has_gate3_corroboration` per survivor; any single `Err` aborts the loop
//!    to `DataGap` naming the failing applicant (T-34-09) — a silently-degraded "count a query
//!    failure as not-capable" would fabricate an exclusion the data cannot support.
//! 5. `capable < population` → `NotApplicable`, one `Finding` (D-07) whose `summary` states BOTH
//!    the exclusion reason (CCOD-company-number-matched-only gate-3 capability, D-05) and a
//!    separate evidence line for the capable subset's own hit rate.
//! 6. `capable == population` → `Verdict::classify` decides `Clear`/`Hit` on `matches =
//!    marker.candidate_apps`.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::{land_flip_capability, land_flip_runs};
use urban_integrity_schema::types::verdict::{EvidenceRow, Finding, Severity, Verdict};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// The milestone's namesake probe — see module doc comment for the full D-01..D-08/D-17 branch
/// structure.
pub struct LandFlipCandidates;

/// `severity` is threaded through by every call site per this plan's severity rule (translated
/// from `scripts/analyst_probe.py:2409-2505`, `probe_land_flip_candidates`) — see each call
/// site's comment for the specific Python line cited, or "no Python analog, defaulted per 35-01
/// convention" where none exists.
fn data_gap(name: &str, severity: Severity, summary: impl Into<String>) -> Finding {
    let summary = summary.into();
    Finding {
        probe: name.to_string(),
        verdict: Verdict::data_gap(summary.clone()),
        summary,
        cause: None,
        severity,
        evidence: vec![],
        attributions: vec![],
        remediation_hint: None,
    }
}

#[async_trait::async_trait]
impl Probe for LandFlipCandidates {
    fn name(&self) -> &'static str {
        "land-flip-candidates"
    }

    /// **Not** searchable by the findings-by-entity index (PIV-02): one aggregate row, and its own source comment records that no id exists on the finding to attach (plan 35-07).
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
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
        "Do any gate-1 survivors also pass gate-3 corroboration, split by structural \
         CCOD-vs-individual capability?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // D-04: the marker check is authoritative and happens FIRST, before any population or
        // capability computation is attempted.
        let marker = match land_flip_runs::get_land_flip_run_marker(db).await {
            Ok(m) => m,
            Err(e) => {
                // No Python analog (Python does not check a land_flip_runs marker) — defaulted
                // to Medium per 35-01's analog-less-DataGap convention.
                return data_gap(
                    self.name(),
                    Severity::Medium,
                    format!("land_flip_runs marker read failed: {e}"),
                );
            }
        };

        let marker = match marker {
            Some(m) => m,
            None => {
                // D-08 reason 1. No Python analog — defaulted to Medium per 35-01's
                // analog-less-DataGap convention.
                return data_gap(
                    self.name(),
                    Severity::Medium,
                    "land-flip detector has not completed a run — no land_flip_runs marker \
                     present",
                );
            }
        };

        // D-03: population = gate-1 survivors. `.max(0)` is a defensive clamp against a `u64`
        // underflow this invariant should never produce given `gate1_no_ppd_match <=
        // applications_scanned` by construction in `run_detection`.
        let population = (marker.applications_scanned - marker.gate1_no_ppd_match).max(0) as u64;

        if population == 0 {
            return match land_flip_runs::ppd_transactions_populated(db).await {
                // No Python analog — defaulted to Medium per 35-01's analog-less-DataGap
                // convention.
                Err(e) => data_gap(
                    self.name(),
                    Severity::Medium,
                    format!("ppd_transactions check failed: {e}"),
                ),
                // D-08 reason 2. Matches Python's own DATA_GAP/INFO for this exact reason
                // (scripts/analyst_probe.py:2432-2438).
                Ok(false) => data_gap(
                    self.name(),
                    Severity::Info,
                    "no staged PPD transactions — the land-flip detector has nothing to match \
                     planning applications against",
                ),
                // D-08 reason 3. No Python analog (Python does not distinguish "PPD staged but
                // no address match" from "PPD staged, zero candidates" — see the CLEAR/INFO
                // branch below) — defaulted to Medium per 35-01's analog-less-DataGap convention.
                Ok(true) => data_gap(
                    self.name(),
                    Severity::Medium,
                    "PPD transactions are staged, but no planning application address matches \
                     any of them",
                ),
            };
        }

        let survivors = match land_flip_runs::scan_land_flip_gate1_survivors_paged(db, 20_000).await
        {
            Ok(s) => s,
            Err(e) => {
                // No Python analog — defaulted to Medium per 35-01's analog-less-DataGap
                // convention.
                return data_gap(
                    self.name(),
                    Severity::Medium,
                    format!("gate1_survivors scan failed: {e}"),
                );
            }
        };

        // D-17: defense-in-depth against 34-03's T-34-08 crash window — a stale-but-complete
        // marker coexisting with a mid-rewrite (or emptied) survivors table. This check runs
        // BEFORE any capability check, so a mismatched table can never be silently treated as a
        // confidently-measured exclusion.
        if survivors.len() as u64 != population {
            // No Python analog — defaulted to Medium per 35-01's analog-less-DataGap convention.
            return data_gap(
                self.name(),
                Severity::Medium,
                format!(
                    "gate-1 survivor count ({}) does not match the marker's recorded population \
                     ({population}) — the land-flip run may have been interrupted; this reading \
                     cannot be trusted",
                    survivors.len()
                ),
            );
        }

        // D-05: capability is the gate-3 predicate itself, evaluated live per survivor. Any
        // single `Err` aborts the loop (T-34-09) — a degraded "treat a query failure as
        // not-capable" would fabricate an exclusion the data cannot support.
        let mut capable: u64 = 0;
        for survivor in &survivors {
            match land_flip_capability::has_gate3_corroboration(db, &survivor.applicant_id).await {
                Ok(true) => capable += 1,
                Ok(false) => {}
                Err(e) => {
                    // No Python analog — defaulted to Medium per 35-01's analog-less-DataGap
                    // convention.
                    return data_gap(
                        self.name(),
                        Severity::Medium,
                        format!(
                            "gate-3 capability check failed for applicant {}: {e}",
                            survivor.applicant_id
                        ),
                    );
                }
            }
        }

        let matches = marker.candidate_apps as u64;

        // The static, probe-authored reproduction note appended to both branches below that
        // reach a non-empty `capable` population — real per-candidate records exist in the
        // separate `land_flip_candidates` table even though this `Finding` does not carry their
        // ids (RPT-04's deliberate aggregate-only classification, D-14's specific-path branch of
        // the pivotability rule). Never a `RecordRef` — no id exists on this `Finding` to attach
        // one to.
        let flips_list_note = EvidenceRow {
            text: "Per-candidate records are not carried on this finding — inspect them via \
                   `urban-integrity-cli flips list` (and `flips show <id>` for a specific \
                   candidate)."
                .to_string(),
            refs: vec![],
        };

        if capable < population {
            let excluded = population - capable;
            // D-07: one Finding, not two — the exclusion reason and the capable subset's own
            // evidence line both live in the same `summary`.
            let reason = format!(
                "{excluded}/{population} gate-1 survivors are structurally ineligible for \
                 gate-3 corroboration — capability is CCOD company-number-matched proprietors \
                 only; director-keyed individuals and OCOD overseas-owned proprietors \
                 (confidence capped at 0.65, status PendingReview) can never pass the >=0.70 \
                 auto_linked/confirmed threshold"
            );
            let verdict = Verdict::classify(population, capable, matches, 1, &reason);
            let summary = format!(
                "{reason}. {matches} of {} produced a candidate this run.",
                plural(capable, "capable survivor", "capable survivors")
            );
            return Finding {
                probe: self.name().to_string(),
                verdict,
                summary,
                cause: None,
                // No Python analog (Python has no capability-split NotApplicable branch) —
                // defaulted to Info per 35-01's analog-less-NotApplicable convention.
                severity: Severity::Info,
                evidence: vec![flips_list_note],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let verdict = Verdict::classify(population, capable, matches, 1, "");
        let summary = format!(
            "{}, all {capable} gate-3-capable; {} this run.",
            plural(population, "gate-1 survivor", "gate-1 survivors"),
            plural(matches, "land-flip candidate", "land-flip candidates")
        );
        // FLIP-04: candidates are review-queue signals, MEDIUM-ceilinged — never HIGH — matching
        // Python's explicit statement (scripts/analyst_probe.py:2495, "By severity (ceiling is
        // MEDIUM — never HIGH)"). Clear matches Python's CLEAR/INFO branch
        // (scripts/analyst_probe.py:2432-2439).
        let severity = match &verdict {
            Verdict::Hit { .. } | Verdict::Weak { .. } => Severity::Medium,
            _ => Severity::Info,
        };
        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
            evidence: vec![flips_list_note],
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::persistence::land_flip_runs::{
        record_land_flip_run, LandFlipRunMarker,
    };

    fn db_tests_enabled() -> bool {
        urban_integrity_test_support::db_tests_enabled()
    }

    /// Builds a `land_flip_runs:current` marker with the fields each fixture cares about; every
    /// other counter defaults to 0 and `finished_at_ms` is stamped to "now" (a genuinely COMPLETE
    /// marker) so callers only need to name `applications_scanned`/`gate1_no_ppd_match`/
    /// `candidate_apps`.
    fn marker(
        applications_scanned: i64,
        gate1_no_ppd_match: i64,
        candidate_apps: i64,
    ) -> LandFlipRunMarker {
        LandFlipRunMarker {
            applications_scanned,
            candidates_written: 0,
            candidate_writes_failed: 0,
            postcode_only_matches: 0,
            gate1_no_ppd_match,
            gate3_query_failed: 0,
            gate3_no_corroboration: 0,
            gate2_no_anchor_date: 0,
            gate2_outside_window: 0,
            candidate_apps,
            finished_at_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// A minimal valid `ppd_transactions` row -- mirrors
    /// `land_flip_runs.rs`'s own `db_ppd_transactions_populated_reflects_row_presence` fixture
    /// shape (SCHEMAFULL, `county` non-optional).
    async fn seed_ppd_row(db: &Surreal<Any>, transaction_id: &str) {
        db.query(
            "CREATE ppd_transactions SET transaction_id = $tid, price_gbp = 100000, \
             date_of_transfer = $date, postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', \
             property_type = 'D', old_new = 'N', tenure = 'F', paon = '1', paon_norm = '1', \
             street = 'Test Street', street_norm = 'teststreet', town = 'Testville', \
             district = 'Test District', county = 'Test County', ppd_category = 'A', \
             district_code = 'W05000001'",
        )
        .bind(("tid", transaction_id.to_string()))
        .bind(("date", chrono::Utc::now()))
        .await
        .expect("seed ppd row")
        .check()
        .expect("seed ppd row check");
    }

    #[tokio::test]
    async fn land_flip_candidates_no_marker_is_data_gap() {
        if !db_tests_enabled() {
            eprintln!("SKIP: land_flip_candidates_no_marker_is_data_gap (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "no_marker",
        )
        .await;
        let mut st = Strategy::default();
        let probe = LandFlipCandidates;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .summary
                .to_lowercase()
                .contains("has not completed a run"),
            "summary must state the detector has not completed a run, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn land_flip_candidates_no_ppd_is_data_gap() {
        if !db_tests_enabled() {
            eprintln!("SKIP: land_flip_candidates_no_ppd_is_data_gap (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "no_ppd",
        )
        .await;
        // population == 0 (applications_scanned == gate1_no_ppd_match); no ppd_transactions row
        // seeded at all.
        record_land_flip_run(&db, &marker(3, 3, 0))
            .await
            .expect("seed marker");

        let mut st = Strategy::default();
        let probe = LandFlipCandidates;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("no staged PPD transactions"),
            "summary must name D-08 reason 2, got: {}",
            finding.summary
        );
        assert!(
            !finding
                .summary
                .contains("no planning application address matches"),
            "reason-2 summary must be distinguishable from reason-3's text, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn land_flip_candidates_zero_survivors_is_data_gap() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: land_flip_candidates_zero_survivors_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "zero_survivors",
        )
        .await;
        // population == 0, but PPD IS staged this time.
        record_land_flip_run(&db, &marker(3, 3, 0))
            .await
            .expect("seed marker");
        seed_ppd_row(&db, "txn-1").await;

        let mut st = Strategy::default();
        let probe = LandFlipCandidates;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .summary
                .contains("no planning application address matches"),
            "summary must name D-08 reason 3, got: {}",
            finding.summary
        );
        assert!(
            !finding.summary.contains("no staged PPD transactions"),
            "reason-3 summary must be distinguishable from reason-2's text, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn land_flip_candidates_survivor_marker_mismatch_is_data_gap() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: land_flip_candidates_survivor_marker_mismatch_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "survivor_mismatch",
        )
        .await;
        // A genuinely COMPLETE marker (finished_at_ms stamped) recording population == 5, but
        // ZERO land_flip_gate1_survivors rows seeded -- simulating 34-03's T-34-08 crash window
        // where a stale-but-complete prior marker coexists with a freshly-emptied survivors
        // table.
        record_land_flip_run(&db, &marker(5, 0, 0))
            .await
            .expect("seed marker");

        let mut st = Strategy::default();
        let probe = LandFlipCandidates;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap for a marker/survivors count mismatch, got {:?}",
            finding.verdict
        );
        assert!(
            !matches!(finding.verdict, Verdict::NotApplicable { .. }),
            "must NOT compute a fabricated NotApplicable against an untrustworthy survivors \
             table, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains('5') && finding.summary.contains('0'),
            "summary must name both the marker's population (5) and the survivor count (0), \
             got: {}",
            finding.summary
        );
    }

    /// Raw `CREATE land_flip_gate1_survivors` seed -- mirrors D-06's shape, not
    /// `write_gate1_survivors` (which DELETEs the whole table first; these fixtures build up
    /// exact per-row shapes across two separate loops in one test).
    async fn seed_survivor_row(
        db: &Surreal<Any>,
        planning_ref: &str,
        applicant_id: &str,
        location_id: &str,
    ) {
        db.query("CREATE land_flip_gate1_survivors CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "planning_ref": planning_ref,
                    "applicant_id": applicant_id,
                    "location_id": location_id,
                }),
            ))
            .await
            .expect("seed land_flip_gate1_survivors row")
            .check()
            .expect("seed land_flip_gate1_survivors row check");
    }

    /// A gate-3-passing `identity_links` row -- confidence 1.0, `auto_linked`, `property:*`
    /// source -- mirroring `land_flip_capability.rs`'s own "direct CCOD-shaped link" fixture
    /// exactly, so `has_gate3_corroboration` returns `true` for `target_node_id`.
    async fn seed_capable_identity_link(
        db: &Surreal<Any>,
        key: &str,
        source_node_id: &str,
        target_node_id: &str,
    ) {
        db.query(
            "CREATE type::record('identity_links', $key) SET \
             source_node_id = $src, target_node_id = $tgt, resolution_type = 'exact_ch_number', \
             confidence = 1.0, source = 'ccod', resolved_at = $resolved_at, \
             evidence_json = '{}', status = 'auto_linked'",
        )
        .bind(("key", key.to_string()))
        .bind(("src", source_node_id.to_string()))
        .bind(("tgt", target_node_id.to_string()))
        .bind(("resolved_at", chrono::Utc::now()))
        .await
        .expect("seed identity_links row")
        .check()
        .expect("seed identity_links row check");
    }

    #[tokio::test]
    async fn land_flip_candidates_not_applicable_when_only_capable_have_no_hit() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: land_flip_candidates_not_applicable_when_only_capable_have_no_hit (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        // A FRESH TestNs -- a distinct seeded population, not a state transition of any other
        // test's data (Pitfall 1).
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "not_applicable_148",
        )
        .await;
        record_land_flip_run(&db, &marker(148, 0, 0))
            .await
            .expect("seed marker");

        // 140 director-keyed survivors, NO identity_links row -- has_gate3_corroboration must
        // return false for each (structurally incapable, D-05).
        for i in 0..140 {
            seed_survivor_row(
                &db,
                &format!("PLAN/{i:04}"),
                &format!("director:app-{i}"),
                &format!("location:loc-{i}"),
            )
            .await;
        }
        // 8 entity-keyed (CCOD-shaped) survivors, each paired with ONE passing identity_links
        // row -- has_gate3_corroboration must return true for each.
        for i in 0..8 {
            let applicant_id = format!("entity:cap-{i}");
            seed_survivor_row(
                &db,
                &format!("PLAN/cap-{i:04}"),
                &applicant_id,
                &format!("location:cap-loc-{i}"),
            )
            .await;
            seed_capable_identity_link(
                &db,
                &format!("cap-link-{i}"),
                &format!("property:x-{i}"),
                &applicant_id,
            )
            .await;
        }

        let mut st = Strategy::default();
        let probe = LandFlipCandidates;
        let finding = probe.run(&db, &mut st).await;

        match &finding.verdict {
            Verdict::NotApplicable { coverage, reason } => {
                assert_eq!(
                    coverage.population(),
                    148,
                    "population must be all 148 gate-1 survivors"
                );
                assert_eq!(
                    coverage.capable(),
                    8,
                    "capable must be exactly the 8 CCOD-company-number-matched survivors -- a \
                     probe returning Clear for the full 148 fails this test by construction"
                );
                assert!(
                    reason.contains("CCOD") || finding.summary.contains("CCOD"),
                    "reason/summary must name the CCOD-only gate-3 registry, got reason={reason}, \
                     summary={}",
                    finding.summary
                );
            }
            other => panic!(
                "expected NotApplicable with population=148/capable=8, got {other:?} \
                 (summary: {})",
                finding.summary
            ),
        }
    }

    #[tokio::test]
    async fn land_flip_candidates_capable_with_hit_is_hit() {
        if !db_tests_enabled() {
            eprintln!("SKIP: land_flip_candidates_capable_with_hit_is_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        // A SEPARATE fresh TestNs from the 148-fixture above (Pitfall 1).
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "capable_with_hit",
        )
        .await;
        record_land_flip_run(&db, &marker(5, 0, 1))
            .await
            .expect("seed marker");

        // All 5 survivors entity-keyed with a passing identity_links row each -- capable == 5 ==
        // population.
        for i in 0..5 {
            let applicant_id = format!("entity:hit-{i}");
            seed_survivor_row(
                &db,
                &format!("PLAN/hit-{i:04}"),
                &applicant_id,
                &format!("location:hit-loc-{i}"),
            )
            .await;
            seed_capable_identity_link(
                &db,
                &format!("hit-link-{i}"),
                &format!("property:hit-{i}"),
                &applicant_id,
            )
            .await;
        }

        let mut st = Strategy::default();
        let probe = LandFlipCandidates;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { matches: 1, .. }),
            "expected Hit with matches=1, got {:?}",
            finding.verdict
        );
    }

    /// The `Clear` branch: `capable == population` (every survivor corroborated) but the run
    /// recorded zero candidate applications, so `Verdict::classify` decides `Clear` on
    /// `matches == 0`. Mirrors `land_flip_candidates_capable_with_hit_is_hit`'s seeding shape
    /// with `candidate_apps: 0` instead of `1`.
    ///
    /// Closes VRD-07 for this probe: without this fixture the `Clear` arm is never proven
    /// reachable, and a verdict that cannot be produced in a test is not implemented.
    #[tokio::test]
    async fn land_flip_candidates_capable_population_zero_matches_is_clear() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: land_flip_candidates_capable_population_zero_matches_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        // A SEPARATE fresh TestNs from every other fixture in this module (Pitfall 1).
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_land_flip",
            "capable_zero_matches",
        )
        .await;
        record_land_flip_run(&db, &marker(5, 0, 0))
            .await
            .expect("seed marker");

        // All 5 survivors entity-keyed with a passing identity_links row each -- capable == 5 ==
        // population, identical to the Hit fixture. Only `candidate_apps` differs.
        for i in 0..5 {
            let applicant_id = format!("entity:clear-{i}");
            seed_survivor_row(
                &db,
                &format!("PLAN/clear-{i:04}"),
                &applicant_id,
                &format!("location:clear-loc-{i}"),
            )
            .await;
            seed_capable_identity_link(
                &db,
                &format!("clear-link-{i}"),
                &format!("property:clear-{i}"),
                &applicant_id,
            )
            .await;
        }

        let mut st = Strategy::default();
        let probe = LandFlipCandidates;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear { .. }),
            "expected Clear (capable == population, matches == 0), got {:?}",
            finding.verdict
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // 5 survivors seeded, each with a passing identity_links row -- capable == population
        // == 5, matches == 0. A fully-corroborated Clear over a real population, the opposite
        // of the zero-population hazard.
        assert_eq!(
            coverage.population(),
            5,
            "population must count all 5 seeded survivor rows"
        );
        assert_eq!(
            coverage.capable(),
            5,
            "capable must equal population -- every survivor has a passing identity link"
        );
    }
}
