//! `OsintConfirmedIntel` Tier-2 probe — ports `probe_osint_confirmed_intel`
//! (`scripts/analyst_probe.py:1233-1292`). Confirmed/auto-linked identity resolutions are direct
//! actionable intelligence — this probe reports their count as the finding itself, no ceiling.
//!
//! Python has no CLEAR/WEAK tier here: `confirmed_links` non-empty ALWAYS returns `"HIT"`
//! (`analyst_probe.py:1288-1292`), never gated on a magnitude. This port matches that exactly —
//! do not invent an intermediate tier Python's original does not have.

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// Python's `high_risk_types` set (`analyst_probe.py:1276-1277`) — resolution types the summary
/// flags explicitly when present among confirmed/auto-linked links.
const HIGH_RISK_TYPES: [&str; 5] = [
    "disqualified_director",
    "phoenix_officer",
    "psc_beneficial_owner",
    "offshore_entity",
    "sanctions_hit",
];

/// Confirmed/auto-linked identity resolutions — see module doc comment.
pub struct OsintConfirmedIntel;

#[async_trait::async_trait]
impl Probe for OsintConfirmedIntel {
    fn name(&self) -> &'static str {
        "osint-confirmed-intel"
    }

    /// **Not** searchable by the findings-by-entity index (PIV-02): its refs are `IdentityLink` record ids, not node ids; the endpoints are in the row TEXT only, so the index cannot follow them without an `identity_links` read it does not do.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesLinksOnly
    }

    /// A bag of named-entity chips, no dedicated motif renderer (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Chips
    }

    fn tier(&self) -> &'static str {
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "Do confirmed or auto-linked OSINT identity resolutions exist, and do any of them carry \
         a high-risk resolution type?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        let rows = match scan::scan_identity_links(db, None, None).await {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result — Probe::run is
                // infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("identity_links scan failed: {e}")),
                    summary: "identity_links scan failed — confirmed intel not available."
                        .to_string(),
                    cause: None,
                    // No direct Python branch analog for a raw scan failure; nearest analog is
                    // analyst_probe.py:2551's generic probe-exception handler (DATA_GAP/INFO).
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: format!("Scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // Filter in Rust, matching Python's own in-Python filter — `scan_identity_links` has no
        // `status IN (...)` SQL parameter, and adding one would be a second near-identical
        // function nothing else needs.
        let confirmed: Vec<_> = rows
            .into_iter()
            .filter(|r| r.status == "confirmed" || r.status == "auto_linked")
            .collect();

        if confirmed.is_empty() {
            let summary = "No confirmed or auto-linked identity resolutions exist yet.".to_string();
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(summary.clone()),
                summary,
                cause: None,
                // analyst_probe.py:1244 — the empty branch's DATA_GAP/MEDIUM pair, 1:1.
                severity: Severity::Medium,
                evidence: vec![EvidenceRow {
                    text: "Once the Phase 17+ OSINT adapters run, confirmed links appear here — \
                           e.g. 'planning applicant J. Smith confirmed as director of 5 dissolved \
                           companies'."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // Grouped by resolution_type, matching Python's `by_type` defaultdict
        // (`analyst_probe.py:1253-1255`). The values are the rows themselves, not a bare count,
        // so D-02's evidence rows can carry per-link pivot targets; `by_type.len()` — the only
        // figure the verdict/summary reads — is unchanged by that.
        let mut by_type: HashMap<&str, Vec<_>> = HashMap::new();
        for row in &confirmed {
            by_type
                .entry(row.resolution_type.as_str())
                .or_default()
                .push(row);
        }

        let risky: Vec<&str> = confirmed
            .iter()
            .filter(|r| HIGH_RISK_TYPES.contains(&r.resolution_type.as_str()))
            .map(|r| r.resolution_type.as_str())
            .collect();

        let mut summary = format!(
            "{} across {}.",
            plural(
                confirmed.len() as u64,
                "confirmed/auto-linked resolution",
                "confirmed/auto-linked resolutions"
            ),
            plural(by_type.len() as u64, "type", "types")
        );
        if !risky.is_empty() {
            let mut distinct: Vec<&str> = risky.clone();
            // D-08: stable `.sort()`, never `.sort_unstable()` — equal elements keep their
            // encounter order, so the reported type list is reproducible run to run.
            distinct.sort();
            distinct.dedup();
            summary.push_str(&format!(
                " {} found ({}).",
                plural(
                    risky.len() as u64,
                    "HIGH-RISK resolution",
                    "HIGH-RISK resolutions"
                ),
                distinct.join(", ")
            ));
        }

        // D-02: the per-type breakdown Python emits as `ev` (`analyst_probe.py:1257-1272`),
        // rebuilt as typed evidence rows. Groups sort by descending size then name, matching
        // Python's `sorted(by_type.items(), key=lambda kv: -len(kv[1]))` with a name tiebreak so
        // equal-sized groups do not reorder run to run.
        let mut groups: Vec<(&str, &Vec<_>)> = by_type.iter().map(|(k, v)| (*k, v)).collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
        let mut evidence: Vec<EvidenceRow> = Vec::new();
        for (rtype, items) in groups {
            evidence.push(EvidenceRow {
                text: format!("{rtype} ({}):", plural(items.len() as u64, "link", "links")),
                refs: vec![],
            });
            for item in items.iter().take(5) {
                evidence.push(EvidenceRow {
                    text: format!(
                        "{:.3}  {}  ->  {}  [{}]",
                        item.confidence, item.source_node_id, item.target_node_id, item.source
                    ),
                    refs: vec![RecordRef {
                        kind: RecordKind::IdentityLink,
                        id: item.id.clone(),
                    }],
                });
            }
            if items.len() > 5 {
                evidence.push(EvidenceRow {
                    text: format!("... and {} more", items.len() - 5),
                    refs: vec![],
                });
            }
        }

        // `matches: len` with `hit_threshold: 1` always fires Hit for ANY non-empty confirmed
        // set — this deliberately reads like a magnitude check, but Python's original has no
        // CLEAR/WEAK branch here (see module doc comment): it is really an unconditional Hit on
        // non-empty.
        let n = confirmed.len() as u64;
        // `Clear` is structurally unreachable here: the `confirmed.is_empty()` guard above
        // returns `DataGap` before this line, so `n >= 1` always, making `matches == n >=
        // hit_threshold (1)` -- unconditionally `Hit`. Proven by
        // `osint_confirmed_intel_clear_is_structurally_unreachable`. This corrects
        // the phase 39 research notes' "Uncertain -- no `== 0` guard found" entry for this probe.
        let verdict = Verdict::classify(n, n, n, 1, "");

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            // analyst_probe.py:1279 — `severity = "CRITICAL" if risky else "HIGH"`, 1:1.
            severity: if risky.is_empty() {
                Severity::High
            } else {
                Severity::Critical
            },
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_identity_link(
        db: &Surreal<Any>,
        idx: usize,
        status: &str,
        resolution_type: &str,
    ) {
        let sni = format!("entity:tier2-conf-src-{idx}");
        let tni = format!("entity:tier2-conf-dst-{idx}");
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: $rtype, \
                 confidence: 0.9, \
                 source: 'ch_psc', \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: '{}', \
                 status: $status \
             }",
        )
        .bind(("sni", sni))
        .bind(("tni", tni))
        .bind(("rtype", resolution_type.to_string()))
        .bind(("status", status.to_string()))
        .await
        .expect("failed to seed identity_links row")
        .check()
        .expect("identity_links CREATE returned a statement error");
    }

    #[tokio::test]
    async fn osint_confirmed_intel_zero_confirmed_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: osint_confirmed_intel_zero_confirmed_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "conf_empty",
        )
        .await;
        let mut st = Strategy::default();

        // Only a pending_review row exists — no confirmed/auto_linked rows.
        seed_identity_link(&db, 0, "pending_review", "exact_company_number").await;

        let probe = OsintConfirmedIntel;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero confirmed/auto_linked rows must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn osint_confirmed_intel_single_confirmed_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: osint_confirmed_intel_single_confirmed_is_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "conf_hit",
        )
        .await;
        let mut st = Strategy::default();

        // Exactly 1 confirmed link — proves there is no hidden magnitude threshold Python
        // doesn't have.
        seed_identity_link(&db, 0, "confirmed", "exact_company_number").await;

        let probe = OsintConfirmedIntel;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "even a single confirmed link must be Hit, got {:?}",
            finding.verdict
        );
    }

    /// `Verdict::Clear` is STRUCTURALLY UNREACHABLE for this probe.
    ///
    /// The phase 39 research notes' VRD-12 inventory flagged this probe "Uncertain -- no `== 0` guard
    /// found; verify". There is a guard. `if confirmed.is_empty() { return ...DataGap... }`
    /// (line ~80) runs before `Verdict::classify(n, n, n, 1, "")` (line ~182), so `n =
    /// confirmed.len()` is always `>= 1` by the time `classify` executes. With `capable ==
    /// population == n` and `matches == n >= 1 >= hit_threshold (1)`, the result is
    /// unconditionally `Hit`. `Clear` requires `matches == 0`, which cannot occur.
    ///
    /// So VRD-12's "assert Coverage values in every Clear-reaching test" obligation is satisfied
    /// VACUOUSLY here -- there is no Clear-reaching test to upgrade. This test makes that a
    /// tested, durable claim rather than an omission a later sweep would read as an oversight.
    /// It drives both sides of the guard and asserts `!Clear` on each.
    #[tokio::test]
    async fn osint_confirmed_intel_clear_is_structurally_unreachable() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: osint_confirmed_intel_clear_is_structurally_unreachable (set URBAN_DB_TESTS=1)"
            );
            return;
        }

        // Side 1: zero confirmed rows -- the guard fires, DataGap, never Clear.
        let db_empty = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "conf_unreachable_empty",
        )
        .await;
        seed_identity_link(&db_empty, 0, "pending_review", "exact_company_number").await;
        let finding_empty = OsintConfirmedIntel
            .run(&db_empty, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding_empty.verdict, Verdict::DataGap { .. }),
            "the confirmed.is_empty() guard must return DataGap, got {:?}",
            finding_empty.verdict
        );
        assert!(
            !matches!(finding_empty.verdict, Verdict::Clear(_)),
            "osint-confirmed-intel must never reach Clear on the guarded (zero-confirmed) path"
        );

        // Side 2: past the guard, n >= 1 forces matches >= hit_threshold -- Hit, never Clear.
        let db_hit = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "conf_unreachable_hit",
        )
        .await;
        seed_identity_link(&db_hit, 0, "confirmed", "exact_company_number").await;
        let finding_hit = OsintConfirmedIntel
            .run(&db_hit, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding_hit.verdict, Verdict::Hit { .. }),
            "past the guard, n >= 1 makes matches >= hit_threshold, forcing Hit, got {:?}",
            finding_hit.verdict
        );
        assert!(
            !matches!(finding_hit.verdict, Verdict::Clear(_)),
            "osint-confirmed-intel must never reach Clear on the post-guard path either"
        );
    }

    #[tokio::test]
    async fn osint_confirmed_intel_high_risk_type_flagged_in_summary() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: osint_confirmed_intel_high_risk_type_flagged_in_summary (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "conf_highrisk",
        )
        .await;
        let mut st = Strategy::default();

        seed_identity_link(&db, 0, "confirmed", "disqualified_director").await;

        let probe = OsintConfirmedIntel;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(finding.verdict, Verdict::Hit { .. }));
        assert!(
            finding.summary.contains("HIGH-RISK"),
            "a high-risk resolution_type must be flagged in the summary, got: {}",
            finding.summary
        );
    }
}
