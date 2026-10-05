//! `WardConcentration` Tier-1 probe — ports `probe_ward_concentration`
//! (`scripts/analyst_probe.py:1067-1123`), closing D4's guard-shape-avoidance requirement.
//!
//! Python's original aggregates server-side via a nested-subquery shape (`SELECT ward, count()
//! FROM (SELECT payload.src_id AS ward, payload.dst_id AS contract, kind FROM graph_events WHERE
//! payload.weight_json CONTAINS 'Represents' GROUP BY ward, contract, kind) WHERE kind =
//! 'EdgeAdded' GROUP BY ward ORDER BY n DESC`). That shape is never ported literally: the guard's
//! `count(` exemption is lexical and does not see inside a `count()`-wrapped subquery, so this
//! nested indexed-`kind=` + unindexed `CONTAINS` shape would pass the guard while carrying the
//! SurrealDB planner-truncation risk (the research notes' Assumption A1). This port routes through
//! `persistence::scan` row-level primitives and dedups/aggregates entirely in Rust — Python's
//! inner `GROUP BY ward, contract, kind` becomes a `HashSet<(String, String)>` insert, which
//! absorbs an append-only re-ingest re-emitting the same `Represents` edge exactly the same way.

use std::collections::{HashMap, HashSet};

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, RemediationHint, Severity, Verdict,
};

use super::{EvidenceScope, MotifKind, Probe, Strategy};

/// `payload.src_id`/`payload.dst_id` as a string, whether SurrealDB handed it back as a plain
/// string or coerced it into a RecordId object (`ward:{code}`-shaped ids match SurrealDB's
/// `table:key` auto-coercion shape). Mirrors `scan.rs`'s private `entity_id_of`/`record_id_str` —
/// neither is exported, so this is a local equivalent per the plan's interfaces note (`scan.rs`'s
/// own visibility is out of scope for this plan).
fn record_id_str(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    let tb = v.get("tb").and_then(|x| x.as_str())?;
    let id = v.get("id")?;
    let id_str = id
        .as_str()
        .map(str::to_owned)
        .or_else(|| id.get("String").and_then(|x| x.as_str()).map(str::to_owned))
        .or_else(|| id.as_i64().map(|n| n.to_string()))?;
    Some(format!("{tb}:{id_str}"))
}

/// D4's guard-shape-avoidance fix (the research notes' Assumption A1) — see module doc comment.
pub struct WardConcentration;

#[async_trait::async_trait]
impl Probe for WardConcentration {
    fn name(&self) -> &'static str {
        "ward-concentration"
    }

    /// Searchable by the findings-by-entity index (PIV-02): the top-wards row carries an `Entity` ref per ward node, with the `ward:` prefix restored (plan 44-21).
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesNodes
    }

    /// A ranked bar chart over a bucketed dimension (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::RankedBars
    }

    fn tier(&self) -> &'static str {
        "Tier-1"
    }

    fn hypothesis(&self) -> &'static str {
        "Are planning applications concentrated in a small number of wards, rather than spread \
         evenly across the LPA?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // D3 (locked): an INDEPENDENT scan — do not reuse a result from identity-integrity or
        // temporal-burst, and do not add a Strategy cache field for this.
        // `scan_latest_entities_by_type`, NOT `scan_entities_by_type` — the latter returns one
        // entry per `graph_events` row, so `.len()` counts INGEST EVENTS, not Contracts. Measured
        // 2026-08-31 on `an_anglesey_aebaa785`: 105,613 events over 45,874 distinct Contracts, so
        // this denominator read 2.3× the real population (bucket (a),
        // `36-DIFFERENTIAL-RESULTS.md`). `temporal-burst` dedups the same population by its own
        // route; `contradictions.rs` sub-check 1a asserts the two agree, so the two fixes are
        // paired by design.
        let total_contracts = match scan::scan_latest_entities_by_type(
            db,
            "NodeAdded",
            "\"type\":\"Contract\"",
        )
        .await
        {
            Ok(rows) => rows.len() as u64,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result — Probe::run
                // is infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Contract scan failed: {e}")),
                    summary: "Contract population scan failed — ward attribution cannot be \
                                  evaluated."
                        .to_string(),
                    cause: None,
                    // No Python analog (this scan-failure branch is Rust-specific
                    // infrastructure handling, PRB-08) — defaulted to Severity::Medium per
                    // 35-01's convention for an analog-less DataGap branch.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("Contract population scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        let edges = match scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"Represents\"")
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Represents edge scan failed: {e}")),
                    summary: "Represents edge scan failed — ward attribution cannot be evaluated."
                        .to_string(),
                    cause: None,
                    // No Python analog — same rationale as the Contract scan failure branch
                    // above, defaulted to Severity::Medium per 35-01's convention.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("Represents edge scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // Dedup (ward, contract) pairs — D4: aggregate in Rust, replacing Python's inner `GROUP
        // BY ward, contract, kind`, which absorbs an append-only re-ingest re-emitting the same
        // Represents edge.
        let mut pairs: HashSet<(String, String)> = HashSet::new();
        for row in &edges {
            let Some(ward) = record_id_str(&row["payload"]["src_id"]) else {
                continue;
            };
            let Some(contract) = record_id_str(&row["payload"]["dst_id"]) else {
                continue;
            };
            pairs.insert((ward, contract));
        }

        let mut wards: HashMap<String, u64> = HashMap::new();
        for (ward, _contract) in &pairs {
            let code = ward.strip_prefix("ward:").unwrap_or(ward);
            *wards.entry(code.to_string()).or_insert(0) += 1;
        }

        let total_attributed: u64 = wards.values().sum();
        let no_ward = total_contracts.saturating_sub(total_attributed);

        // Verdict: WEAK if wards non-empty, else DATA_GAP — this probe NEVER returns Clear or Hit
        // in Python (confirmed: `"WEAK" if wards else "DATA_GAP"`, analyst_probe.py:1120).
        if wards.is_empty() {
            let summary = format!(
                "No Represents edges found — ward attribution unavailable ({} total).",
                super::plural(total_contracts, "Contract", "Contracts")
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(summary.clone()),
                summary,
                cause: None,
                // Python's probe_ward_concentration (analyst_probe.py:1120) reports "LOW" for
                // this whole probe unconditionally — WEAK or DATA_GAP alike.
                severity: Severity::Low,
                evidence: vec![EvidenceRow {
                    text: format!(
                        "{} evaluated; zero carry a Represents edge.",
                        super::plural(total_contracts, "Contract", "Contracts")
                    ),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let mut top_wards: Vec<(&String, &u64)> = wards.iter().collect();
        top_wards.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let top_10: Vec<String> = top_wards
            .iter()
            .take(10)
            .map(|(ward, count)| format!("{ward}: {count}"))
            .collect();

        let summary = format!(
            "{} across {} of {} total.",
            super::plural(wards.len() as u64, "ward", "wards"),
            super::plural(
                total_attributed,
                "ward-attributed contract",
                "ward-attributed contracts"
            ),
            super::plural(total_contracts, "contract", "contracts"),
        );

        // The ward codes in `wards` have had their `ward:` prefix stripped (above), but the graph
        // node id keeps it — so the refs restore it rather than pointing at an id no node has.
        // Guarded rather than blindly prefixed: a code that somehow arrives already prefixed must
        // not become `ward:ward:…`.
        let top_ward_refs: Vec<RecordRef> = top_wards
            .iter()
            .take(10)
            .map(|(ward, _)| RecordRef {
                kind: RecordKind::Entity,
                id: if ward.starts_with("ward:") {
                    (*ward).clone()
                } else {
                    format!("ward:{ward}")
                },
            })
            .collect();
        let mut evidence = vec![EvidenceRow {
            text: format!("Top wards: {}.", top_10.join(", ")),
            refs: top_ward_refs,
        }];
        // D-10's typed remediation trigger — replaces Python's
        // `re.match(r"(\d+) ward\(s\)", wc_finding.summary)` regex (analyst_probe.py:2846), which
        // D-09 forbids porting. Only this WEAK branch (the one that has actually computed
        // `no_ward`) may set this; every other branch above sets `remediation_hint: None`.
        let remediation_hint = if no_ward > 0 {
            evidence.push(EvidenceRow {
                text: format!(
                    "{} lack ward attribution — limits IMD weighting.",
                    super::plural(no_ward, "contract", "contracts")
                ),
                refs: vec![],
            });
            Some(RemediationHint::WardsUnattributed(no_ward))
        } else {
            None
        };

        // `matches: 1` is a deliberate non-zero sentinel routing past `classify`'s `matches == 0
        // -> Clear` branch — there is no magnitude threshold in Python's "WEAK" if wards else
        // "DATA_GAP" vocabulary; any non-empty `wards` map IS the WEAK signal itself, not a count
        // crossing a cutoff. This is a non-obvious use of the `classify` API, documented here
        // explicitly per the plan.
        let verdict = Verdict::classify(total_contracts, total_contracts, 1, u64::MAX, "");

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            // Python's probe_ward_concentration (analyst_probe.py:1120) reports "LOW" for this
            // whole probe unconditionally — WEAK or DATA_GAP alike, matching the DataGap branch
            // above.
            severity: Severity::Low,
            evidence,
            attributions: vec![],
            remediation_hint,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, RepresentsData};
    use urban_integrity_schema::types::node::{ContractData, NodeWeight};

    async fn seed_contract_event(db: &Surreal<Any>, entity_id: &str) {
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
            applicant_provenance: None,
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

    async fn seed_represents_edge(
        db: &Surreal<Any>,
        event_id: &str,
        ward_id: &str,
        contract_id: &str,
    ) {
        let edge = EdgeWeight::Represents {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: RepresentsData {
                details: String::new(),
            },
        };
        let weight_json = serde_json::to_string(&edge).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": { "src_id": ward_id, "dst_id": contract_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed represents edge");
    }

    #[tokio::test]
    async fn ward_concentration_zero_represents_edges_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ward_concentration_zero_represents_edges_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Some Contract nodes present, but zero Represents edges.
        seed_contract_event(&db, "GB-CF:WC-NOEDGE-1").await;
        seed_contract_event(&db, "GB-CF:WC-NOEDGE-2").await;

        let probe = WardConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero Represents edges must be DataGap regardless of Contract population, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn ward_concentration_multiple_wards_unattributed_contracts_is_weak() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ward_concentration_multiple_wards_unattributed_contracts_is_weak (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "multi_ward",
        )
        .await;
        let mut st = Strategy::default();

        // 5 Contracts total; 3 attributed across 2 wards, 2 left unattributed.
        for i in 0..5 {
            seed_contract_event(&db, &format!("GB-CF:WC-WEAK-{i}")).await;
        }
        seed_represents_edge(&db, "edge-rep-1", "ward:W05000001", "GB-CF:WC-WEAK-0").await;
        seed_represents_edge(&db, "edge-rep-2", "ward:W05000001", "GB-CF:WC-WEAK-1").await;
        seed_represents_edge(&db, "edge-rep-3", "ward:W05000002", "GB-CF:WC-WEAK-2").await;

        let probe = WardConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "multiple wards with attributed contracts must be Weak, never Clear/Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains(&5u64.to_string()),
            "evidence must state the total contract population figure, got: {}",
            finding.summary
        );
        assert!(
            !matches!(finding.verdict, Verdict::Clear(_)),
            "ward-concentration must never reach Clear -- matches is a hardcoded 1 sentinel (see this file's own doc comment), so classify's matches==0 branch can never fire regardless of total_contracts"
        );

        // The top-wards row must be pivotable, and its refs must carry the `ward:` prefix the
        // graph node ids actually use — the probe's own map keys have it stripped, so a ref built
        // straight from those keys would name nodes that do not exist.
        let top_row = finding
            .evidence
            .iter()
            .find(|row| !row.refs.is_empty())
            .expect("the top-wards row must carry refs");
        let ids: Vec<&str> = top_row.refs.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"ward:W05000001") && ids.contains(&"ward:W05000002"),
            "both attributed wards must be referenced with their ward: prefix, got {ids:?}"
        );
        assert!(
            !ids.iter().any(|id| id.starts_with("ward:ward:")),
            "a ward id must never be double-prefixed, got {ids:?}"
        );
    }

    #[tokio::test]
    async fn ward_concentration_duplicate_edge_is_deduped_not_double_counted() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ward_concentration_duplicate_edge_is_deduped_not_double_counted (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "dedup_ward",
        )
        .await;
        let mut st = Strategy::default();

        seed_contract_event(&db, "GB-CF:WC-DEDUP-1").await;
        // The SAME (ward, contract) Represents edge seeded TWICE, simulating an append-only
        // re-ingest re-emitting the identical edge.
        seed_represents_edge(&db, "edge-rep-dup-1", "ward:W05000003", "GB-CF:WC-DEDUP-1").await;
        seed_represents_edge(&db, "edge-rep-dup-2", "ward:W05000003", "GB-CF:WC-DEDUP-1").await;

        let probe = WardConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "a single attributed contract (deduped) must be Weak, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .summary
                .contains("across 1 ward-attributed contract"),
            "the duplicate Represents edge must be deduped to ONE attributed contract, not \
             two, got: {}",
            finding.summary
        );
        assert!(
            !matches!(finding.verdict, Verdict::Clear(_)),
            "ward-concentration must never reach Clear -- matches is a hardcoded 1 sentinel (see this file's own doc comment), so classify's matches==0 branch can never fire regardless of total_contracts"
        );
    }
}
