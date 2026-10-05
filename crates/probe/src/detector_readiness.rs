//! `DetectorReadiness` Tier-1 probe — ports `probe_detector_readiness`
//! (`scripts/analyst_probe.py:875-981`), naming WHICH motif detector (Phoenix loop / Nepotism
//! bridge / Address cluster) is blocked and by WHICH specific missing term, never collapsing that
//! into a generic node-type shortfall count.
//!
//! Two guard-shape avoidances are load-bearing here (D2/D4, the phase 32 design notes):
//!
//! - `_dissolution_date_spread` ports as a **full paged scan** through `persistence::scan`,
//!   dropping Python's `sample_limit=20000` entirely (D2, locked). The evidence text says so
//!   explicitly via `plural()` — "N dissolution events examined (full scan)" — never implying a
//!   sample and never D-03's banned mechanical pluralisation suffix.
//! - Every term routes through `scan::scan_events_by_type`/`scan_events_by_kind`/
//!   `scan_entities_by_type` and is counted/filtered in Rust; no raw aggregate SurrealQL is
//!   written anywhere in this file (D4).

use std::collections::{HashMap, HashSet};

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::{detector_runs, scan};
use urban_integrity_schema::types::verdict::{EvidenceRow, Finding, Severity, Verdict};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// `payload.src_id`/`payload.dst_id`/`payload.entity_id` as a string, whether SurrealDB handed it
/// back as a plain string or coerced it into a RecordId object (e.g. `director:{slug}`,
/// `political_actor:{slug}`, `PA:{slug}` all match SurrealDB's `table:key` auto-coercion shape).
/// Mirrors `scan.rs`'s private `entity_id_of`/`record_id_str` — neither is exported, so this is a
/// local equivalent per the plan's interfaces note (`scan.rs`'s own visibility is out of scope).
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

/// The three honestly-distinguished states `tier_b_marker_line` composes (Phase 46, D-05) —
/// private to this file, since it composes the two `detector_runs` functions' answers at the
/// call site rather than widening either one into a new public core type.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TierBRunSignal {
    /// At least one well-formed `pipeline == "core"` history row exists — the healthy,
    /// post-46-04-migration steady state. Carries that row's genuine per-run detail (the most
    /// recent by `started_at_ms`).
    Recorded {
        started_at_ms: i64,
        motif_count: i64,
    },
    /// `detector_has_ever_run` is `true` but `list_detector_runs` holds no `pipeline == "core"`
    /// row — the exact transitional state an analysis is in immediately after Phase 46 ships,
    /// until the next bake. Must never be phrased so it could be mistaken for `Recorded`.
    LegacyOnly,
    /// `detector_has_ever_run` is `false` — the detector has genuinely never run.
    Never,
}

/// Compose the two `detector_runs` functions' answers into one [`TierBRunSignal`] — call-site
/// composition, never a widening of either function's own contract. `list_detector_runs` still
/// excludes the legacy row from its own return value exactly as before.
async fn compute_tier_b_run_signal(db: &Surreal<Any>) -> Result<TierBRunSignal, String> {
    let has_run = detector_runs::detector_has_ever_run(db)
        .await
        .map_err(|e| e.to_string())?;
    if !has_run {
        return Ok(TierBRunSignal::Never);
    }
    let runs = detector_runs::list_detector_runs(db)
        .await
        .map_err(|e| e.to_string())?;
    match runs
        .iter()
        .filter(|r| r.pipeline == "core")
        .max_by_key(|r| r.started_at_ms)
    {
        Some(r) => Ok(TierBRunSignal::Recorded {
            started_at_ms: r.started_at_ms,
            motif_count: r.motif_count,
        }),
        None => Ok(TierBRunSignal::LegacyOnly),
    }
}

/// D-03: the "did Tier-B actually run" signal relocated from `precomputed-alerts` — an evidence
/// row here, not a `Verdict` state. Takes `Result<TierBRunSignal, String>` rather than a raw
/// `surrealdb::Result<_>` so all four branches (including the error branch) are unit-testable
/// without constructing a real `surrealdb::Error`; the call site converts via
/// `compute_tier_b_run_signal`.
fn tier_b_marker_line(signal: Result<TierBRunSignal, String>) -> String {
    match signal {
        Ok(TierBRunSignal::Recorded {
            started_at_ms,
            motif_count,
        }) => format!(
            "Tier-B motif detector has run (last run started_at_ms={started_at_ms}, \
             motif_count={motif_count})."
        ),
        Ok(TierBRunSignal::LegacyOnly) => "Tier-B motif detector has run at least once, but \
             only a pre-migration detector_runs record exists (no per-run history yet — this \
             analysis has not been re-baked since Phase 46 shipped)."
            .to_string(),
        Ok(TierBRunSignal::Never) => {
            "Tier-B motif detector has never run (no detector_runs record present).".to_string()
        }
        Err(e) => format!("detector_runs read failed: {e} — Tier-B run status unknown."),
    }
}

/// How a motif term's readiness is counted. Mirrors Python's `_MOTIF_TERMS` probe tuples
/// (`analyst_probe.py:753-816`) as a closed Rust enum instead of a `(event_kind, [clause, ...])`
/// tuple, since every clause shape Python used maps onto exactly one of these four cases.
#[derive(Clone, Copy)]
enum TermSpec {
    /// The common case: `kind`, and a single `weight_json CONTAINS marker` clause.
    Marker(&'static str, &'static str),
    /// `political-actor-edges`: an `EdgeAdded` row whose `src_id` OR `dst_id` starts with one of
    /// 4 political-actor prefixes — a PREFIX check on the edge endpoints, not a `CONTAINS` on
    /// `weight_json`.
    EdgePrefix,
    /// `terminal-status`: `NodeAdded` + `"type":"Entity"` AND (`"type":"Dissolved"` OR
    /// `"type":"InLiquidation"`) — two ANDed `CONTAINS` checks against the SAME row.
    TerminalStatus,
    /// `officer-directs-edges`: resolve Officer entity_ids first; if none exist, short-circuit to
    /// 0 without scanning `Directs` edges at all (Python's own short-circuit,
    /// `_count_motif_term:827-838`).
    Derived,
}

struct TermDef {
    key: &'static str,
    label: &'static str,
    spec: TermSpec,
    role: &'static str,
}

struct MotifGroup {
    name: &'static str,
    source: &'static str,
    terms: &'static [TermDef],
}

const PHOENIX_TERMS: &[TermDef] = &[
    TermDef {
        key: "officer-nodes",
        label: "Officer nodes",
        spec: TermSpec::Marker("NodeAdded", "\"type\":\"Officer\""),
        role: "the seed's officer hop",
    },
    TermDef {
        key: "officer-directs-edges",
        label: "Directs edges touching an Officer node",
        spec: TermSpec::Derived,
        role: "connects officers to their companies",
    },
    TermDef {
        key: "terminal-status",
        label: "Dissolved/InLiquidation entities",
        spec: TermSpec::TerminalStatus,
        role: "the failed counterparty the loop points at",
    },
    TermDef {
        key: "awardedto-edges",
        label: "AwardedTo edges",
        spec: TermSpec::Marker("EdgeAdded", "\"type\":\"AwardedTo\""),
        role: "supplies the award date anchoring the 180-day window",
    },
];

const NEPOTISM_TERMS: &[TermDef] = &[
    TermDef {
        key: "political-actor-nodes",
        label: "PoliticalActor nodes",
        spec: TermSpec::Marker("NodeAdded", "\"type\":\"PoliticalActor\""),
        role: "the BFS start point",
    },
    TermDef {
        key: "officer-nodes",
        label: "Officer nodes",
        spec: TermSpec::Marker("NodeAdded", "\"type\":\"Officer\""),
        role: "the BFS target",
    },
    TermDef {
        key: "political-actor-edges",
        label: "Edges incident to a PoliticalActor",
        spec: TermSpec::EdgePrefix,
        role: "the only edges the BFS can leave the start point on",
    },
];

const ADDRESS_CLUSTER_TERMS: &[TermDef] = &[
    TermDef {
        key: "entity-nodes",
        label: "Entity nodes",
        spec: TermSpec::Marker("NodeAdded", "\"type\":\"Entity\""),
        role: "the co-located bidders",
    },
    TermDef {
        key: "location-nodes",
        label: "Location nodes",
        spec: TermSpec::Marker("NodeAdded", "\"type\":\"Location\""),
        role: "the shared address",
    },
    TermDef {
        key: "locatedat-edges",
        label: "LocatedAt edges",
        spec: TermSpec::Marker("EdgeAdded", "\"type\":\"LocatedAt\""),
        role: "binds bidders to addresses",
    },
];

const MOTIF_GROUPS: &[MotifGroup] = &[
    MotifGroup {
        name: "Phoenix loop",
        source: "crates/detector/src/motif/phoenix.rs:57-131",
        terms: PHOENIX_TERMS,
    },
    MotifGroup {
        name: "Nepotism bridge",
        source: "crates/detector/src/motif/nepotism.rs:29-60",
        terms: NEPOTISM_TERMS,
    },
    MotifGroup {
        name: "Address cluster",
        source: "crates/detector/src/motif/cluster.rs:107-190",
        terms: ADDRESS_CLUSTER_TERMS,
    },
];

/// The 4 OSINT node types whose presence is reported (not gating), matching Python's
/// `osint_types` tuple (`analyst_probe.py:876`).
const OSINT_TYPES: &[&str] = &["Psc", "Charity", "Property", "OffshoreEntity"];

/// Counts one motif term. Every branch routes through `persistence::scan` primitives — no raw
/// aggregate SurrealQL (D4).
async fn count_term(db: &Surreal<Any>, spec: TermSpec) -> surrealdb::Result<u64> {
    match spec {
        TermSpec::Marker(kind, marker) => {
            Ok(scan::scan_events_by_type(db, kind, marker).await?.len() as u64)
        }
        TermSpec::TerminalStatus => {
            let rows = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Entity\"").await?;
            Ok(rows
                .iter()
                .filter(|r| {
                    let wj = r["payload"]["weight_json"].as_str().unwrap_or("");
                    wj.contains("\"type\":\"Dissolved\"")
                        || wj.contains("\"type\":\"InLiquidation\"")
                })
                .count() as u64)
        }
        TermSpec::EdgePrefix => {
            let rows = scan::scan_events_by_kind(db, "EdgeAdded").await?;
            Ok(rows
                .iter()
                .filter(|r| {
                    let src = record_id_str(&r["payload"]["src_id"]).unwrap_or_default();
                    let dst = record_id_str(&r["payload"]["dst_id"]).unwrap_or_default();
                    src.starts_with("political_actor:")
                        || dst.starts_with("political_actor:")
                        || src.starts_with("PA:")
                        || dst.starts_with("PA:")
                })
                .count() as u64)
        }
        TermSpec::Derived => {
            let officers =
                scan::scan_entities_by_type(db, "NodeAdded", "\"type\":\"Officer\"").await?;
            if officers.is_empty() {
                return Ok(0);
            }
            let officer_ids: HashSet<String> = officers.into_iter().map(|(id, _)| id).collect();
            let edges = scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"Directs\"").await?;
            Ok(edges
                .iter()
                .filter(|r| {
                    let src = record_id_str(&r["payload"]["src_id"]).unwrap_or_default();
                    let dst = record_id_str(&r["payload"]["dst_id"]).unwrap_or_default();
                    officer_ids.contains(&src) || officer_ids.contains(&dst)
                })
                .count() as u64)
        }
    }
}

/// Port of `_dissolution_date_spread` (`analyst_probe.py:846-872`) — D2, LOCKED: a FULL paged
/// scan, dropping Python's `sample_limit=20000` entirely. Returns
/// `(distinct_dates, rows_examined)` where `rows_examined` is the total row count actually
/// scanned (a full-scan figure, never a sample size — say so at the call site, per D2).
async fn dissolution_date_spread(db: &Surreal<Any>) -> surrealdb::Result<(usize, usize)> {
    let rows = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Dissolved\"").await?;
    const DATE_MARKER: &str = "\"type\":\"Dissolved\",\"content\":\"";
    let mut dates: HashSet<String> = HashSet::new();
    for row in &rows {
        let wj = row["payload"]["weight_json"].as_str().unwrap_or("");
        if let Some(pos) = wj.find(DATE_MARKER) {
            let start = pos + DATE_MARKER.len();
            if let Some(date) = wj.get(start..start + 10) {
                dates.insert(date.to_string());
            }
        }
    }
    Ok((dates.len(), rows.len()))
}

/// The motif-term readiness table (VRD-06's specific-term-naming fix, GRD-03/04).
pub struct DetectorReadiness;

#[async_trait::async_trait]
impl Probe for DetectorReadiness {
    fn name(&self) -> &'static str {
        "detector-readiness"
    }

    /// **Not** searchable by the findings-by-entity index (PIV-02): counts of detector inputs; no id is ever interpolated.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    ///
    /// Re-confirmed genuinely aggregate-only by Phase 49's EVM-01 audit, 2026-09-17 -- see
    /// plan 49-10. `count_term`'s `EdgePrefix`/`Derived` branches do extract per-row `src`/
    /// `dst` ids transiently (`record_id_str`), but only to answer a boolean term-presence
    /// question; the reported `Coverage::population()` is `term_counts.len()` (fixed at 9, the
    /// static `MOTIF_GROUPS` term catalog), never a count of those extracted ids. Contrast
    /// `identity-integrity`, this plan's other Task 2 file, whose population IS a discarded-id
    /// entity count.
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
        "Is every term each motif detector needs actually present in the graph, so the \
         detector can fire at all — and did the Tier-B motif detector actually run?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // Count every distinct term once (officer-nodes appears in both Phoenix and Nepotism —
        // counted once via this HashMap keyed on term_key, reused by both motifs when building
        // evidence, matching Python's own dedup-by-key behaviour).
        let mut term_counts: HashMap<&'static str, u64> = HashMap::new();
        let mut term_labels: HashMap<&'static str, &'static str> = HashMap::new();
        for group in MOTIF_GROUPS {
            for term in group.terms {
                if term_counts.contains_key(term.key) {
                    continue;
                }
                match count_term(db, term.spec).await {
                    Ok(n) => {
                        term_counts.insert(term.key, n);
                        term_labels.insert(term.key, term.label);
                    }
                    Err(e) => {
                        // A scan failure is a DataGap Finding, not a propagated Result —
                        // Probe::run is infallible by design (PRB-08).
                        return Finding {
                            probe: self.name().to_string(),
                            verdict: Verdict::data_gap(format!(
                                "scan failed while counting '{}': {e}",
                                term.label
                            )),
                            summary: format!(
                                "Detector-readiness scan failed while counting '{}' — motif \
                                 readiness cannot be evaluated.",
                                term.label
                            ),
                            cause: None,
                            // No direct Python branch analog for a raw scan failure; nearest
                            // analog is analyst_probe.py:2551's generic probe-exception
                            // handler (DATA_GAP/INFO).
                            severity: Severity::Info,
                            evidence: vec![EvidenceRow {
                                text: format!("Term-count scan error on '{}': {e}", term.label),
                                refs: vec![],
                            }],
                            attributions: vec![],
                            remediation_hint: None,
                        };
                    }
                }
            }
        }

        let mut osint_present: Vec<&'static str> = Vec::new();
        for t in OSINT_TYPES {
            let marker = format!("\"type\":\"{t}\"");
            match scan::scan_events_by_type(db, "NodeAdded", &marker).await {
                Ok(rows) => {
                    if !rows.is_empty() {
                        osint_present.push(t);
                    }
                }
                Err(e) => {
                    return Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::data_gap(format!(
                            "OSINT presence scan failed for '{t}': {e}"
                        )),
                        summary: format!(
                            "OSINT presence scan failed for '{t}' — motif readiness cannot be \
                             evaluated."
                        ),
                        cause: None,
                        // No direct Python branch analog for a raw scan failure; nearest analog
                        // is analyst_probe.py:2551's generic probe-exception handler
                        // (DATA_GAP/INFO).
                        severity: Severity::Info,
                        evidence: vec![EvidenceRow {
                            text: format!("OSINT presence scan error on '{t}': {e}"),
                            refs: vec![],
                        }],
                        attributions: vec![],
                        remediation_hint: None,
                    };
                }
            }
        }

        // A term can be satisfied on count and still unusable. This does NOT block a motif (the
        // data is there) — only annotates evidence, per D2/T-32-02.
        let mut degraded_note: Option<String> = None;
        if term_counts.get("terminal-status").copied().unwrap_or(0) > 0 {
            match dissolution_date_spread(db).await {
                Ok((distinct_dates, rows_examined)) => {
                    if distinct_dates == 1 && rows_examined > 1 {
                        degraded_note = Some(format!(
                            "terminal-status — all {} examined (full scan) share one date, a \
                             stream-arrival stamp rather than date_of_cessation, so Phoenix's \
                             180-day window is anchored at ingest day",
                            plural(
                                rows_examined as u64,
                                "dissolution event",
                                "dissolution events"
                            )
                        ));
                    }
                }
                Err(e) => {
                    return Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::data_gap(format!(
                            "dissolution-date-spread full scan failed: {e}"
                        )),
                        summary: "dissolution-date-spread full scan failed — motif readiness \
                                  cannot be evaluated."
                            .to_string(),
                        cause: None,
                        // No direct Python branch analog for a raw scan failure; nearest analog
                        // is analyst_probe.py:2551's generic probe-exception handler
                        // (DATA_GAP/INFO).
                        severity: Severity::Info,
                        evidence: vec![EvidenceRow {
                            text: format!("dissolution-date-spread scan error: {e}"),
                            refs: vec![],
                        }],
                        attributions: vec![],
                        remediation_hint: None,
                    };
                }
            }
        }

        // D-02: the OSINT-presence line and the degraded-term annotation are evidence rows,
        // not summary prose — Python appends both to `summary` (`analyst_probe.py:951-958`),
        // which is exactly the merged-prose shape D-02 splits apart.
        let osint_line = if osint_present.is_empty() {
            "OSINT node types (PSC, Charity, Property, OffshoreEntity) absent — Phase 17+ not \
             yet run."
                .to_string()
        } else {
            format!("OSINT node types present: {}.", osint_present.join(", "))
        };

        // `(motif_name, motif_source, [(missing_term_label, missing_term_role), ...])` — `source`
        // and `role` are both surfaced in the blocked-motif description below (mirroring
        // Python's per-motif `f"{motif} ({spec['source']}) — ..."` citation and per-term role
        // line, `analyst_probe.py:947-963`), not merely carried for documentation.
        let mut blocked: Vec<(
            &'static str,
            &'static str,
            Vec<(&'static str, &'static str)>,
        )> = Vec::new();
        for group in MOTIF_GROUPS {
            let missing: Vec<(&'static str, &'static str)> = group
                .terms
                .iter()
                .filter(|t| term_counts.get(t.key).copied().unwrap_or(0) == 0)
                .map(|t| (t.label, t.role))
                .collect();
            if !missing.is_empty() {
                blocked.push((group.name, group.source, missing));
            }
        }

        // Python's `ev` block (`analyst_probe.py:911-934`): every term grouped under the motif
        // that needs it, so a blocked motif's full shortfall reads at a glance. No pivot refs —
        // each row is a count over a node type, not a single record.
        let mut evidence: Vec<EvidenceRow> = Vec::new();
        for group in MOTIF_GROUPS {
            let missing_here = group
                .terms
                .iter()
                .filter(|t| term_counts.get(t.key).copied().unwrap_or(0) == 0)
                .count();
            let state = if missing_here == 0 {
                "all terms present".to_string()
            } else {
                format!(
                    "BLOCKED — {missing_here} of {} terms missing",
                    group.terms.len()
                )
            };
            evidence.push(EvidenceRow {
                text: format!("{} ({}) — {state}", group.name, group.source),
                refs: vec![],
            });
            for term in group.terms {
                let count = term_counts.get(term.key).copied().unwrap_or(0);
                let degraded_here = term.key == "terminal-status" && degraded_note.is_some();
                let mark = if count == 0 {
                    "MISSING"
                } else if degraded_here {
                    "DEGRADED"
                } else {
                    "ok"
                };
                evidence.push(EvidenceRow {
                    text: format!("{count}  {}  {mark}  {}", term.label, term.role),
                    refs: vec![],
                });
            }
        }
        if let Some(note) = degraded_note.as_ref() {
            evidence.push(EvidenceRow {
                text: format!("Degraded terms (present but not usable): {note}."),
                refs: vec![],
            });
        }
        evidence.push(EvidenceRow {
            text: osint_line,
            refs: vec![],
        });

        // D-03: surface the Tier-B "did it run" signal as an evidence row on every exit path
        // (both the blocked-motifs DataGap Finding below and the all-present Clear Finding at
        // the end of this function) — this read does NOT gate detector-readiness's own verdict,
        // matching the degraded_note precedent above ("annotate, don't gate").
        let tier_b_line = tier_b_marker_line(compute_tier_b_run_signal(db).await);
        evidence.push(EvidenceRow {
            text: tier_b_line,
            refs: vec![],
        });

        if !blocked.is_empty() {
            let mut all_missing: Vec<&'static str> = blocked
                .iter()
                .flat_map(|(_, _, m)| m.iter().map(|(label, _role)| *label))
                .collect();
            // D-08: stable `.sort()`, never `.sort_unstable()`.
            all_missing.sort();
            all_missing.dedup();
            let blocked_desc: Vec<String> = blocked
                .iter()
                .map(|(name, source, missing)| {
                    let terms = missing
                        .iter()
                        .map(|(label, role)| format!("{label} [{role}]"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{name} ({source}) (missing: {terms})")
                })
                .collect();
            let summary = format!(
                "{} of {} motif detectors cannot fire. Missing terms: {}. Blocked: {}. A motif \
                 needs every one of its terms — supplying one missing term does not unblock a \
                 motif that is short two.",
                blocked.len(),
                MOTIF_GROUPS.len(),
                all_missing.join(", "),
                blocked_desc.join("; ")
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(format!(
                    "{} of {} motif detectors blocked: {}",
                    blocked.len(),
                    MOTIF_GROUPS.len(),
                    blocked_desc.join("; ")
                )),
                summary,
                cause: None,
                // analyst_probe.py:963 — the blocked branch's DATA_GAP/HIGH pair, 1:1.
                severity: Severity::High,
                evidence,
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // All terms present. population == capable == the fixed number of distinct terms this
        // table tracks (9 — 4 Phoenix + 3 Nepotism, minus the shared officer-nodes term, + 3
        // Address cluster — every distinct key in `term_counts` at this point). Derived from
        // `term_counts.len()` rather than a hard-coded literal so a future term addition to
        // MOTIF_GROUPS cannot silently desync this figure from the table it describes.
        let n_terms = term_counts.len() as u64;
        Finding {
            probe: self.name().to_string(),
            verdict: Verdict::classify(n_terms, n_terms, 0, u64::MAX, ""),
            summary: "All motif terms present — all three detectors can fire.".to_string(),
            cause: None,
            // analyst_probe.py:981 — the all-present branch's CLEAR/INFO pair, 1:1.
            severity: Severity::Info,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{
        AwardedToData, DirectsData, EdgeMeta, EdgeWeight, LocatedAtData,
    };
    use urban_integrity_schema::types::node::{
        EntityData, EntityStatus, LocationData, NodeWeight, OfficerData, PoliticalActorData,
    };

    #[test]
    fn tier_b_marker_line_present_names_run_state_and_counts() {
        let line = tier_b_marker_line(Ok(TierBRunSignal::Recorded {
            started_at_ms: 123,
            motif_count: 5,
        }));
        assert!(line.contains("Tier-B"), "got: {line}");
        assert!(line.contains('5'), "got: {line}");
    }

    #[test]
    fn tier_b_marker_line_legacy_only_is_distinct_from_recorded_and_never() {
        let line = tier_b_marker_line(Ok(TierBRunSignal::LegacyOnly));
        assert!(
            line.contains("Tier-B motif detector has run"),
            "got: {line}"
        );
        assert!(line.contains("pre-migration"), "got: {line}");
        assert!(!line.contains("never run"), "got: {line}");
    }

    #[test]
    fn tier_b_marker_line_absent_says_never_run() {
        let line = tier_b_marker_line(Ok(TierBRunSignal::Never));
        assert!(line.contains("never run"), "got: {line}");
    }

    #[test]
    fn tier_b_marker_line_error_annotates_without_panicking() {
        let line = tier_b_marker_line(Err("connection refused".to_string()));
        assert!(line.contains("connection refused"), "got: {line}");
        assert!(line.contains("unknown"), "got: {line}");
    }

    async fn seed_node_event(db: &Surreal<Any>, entity_id: &str, weight: &NodeWeight) {
        let weight_json = serde_json::to_string(weight).unwrap();
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
            .expect("failed to seed node event");
    }

    async fn seed_edge_event(
        db: &Surreal<Any>,
        event_id: &str,
        src_id: &str,
        dst_id: &str,
        weight: &EdgeWeight,
    ) {
        let weight_json = serde_json::to_string(weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed edge event");
    }

    fn edge_meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    /// Seeds every term in every motif group at least once, plus a SINGLE Dissolved row (so
    /// terminal-status is non-zero without tripping the degraded annotation — that requires
    /// `rows_examined > 1`).
    async fn seed_all_terms_present(db: &Surreal<Any>) {
        // officer-nodes (Phoenix + Nepotism, shared).
        seed_node_event(
            db,
            "director:jane-officer",
            &NodeWeight::Officer(OfficerData {
                name: "Jane Officer".into(),
                officer_role: Some("director".into()),
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        )
        .await;

        // terminal-status + entity-nodes (Entity + Dissolved, same row satisfies both terms).
        seed_node_event(
            db,
            "GB-COH:DR-DISSOLVED-1",
            &NodeWeight::Entity(EntityData {
                name: "Dissolved Co".into(),
                company_number: Some("DR000001".into()),
                status: EntityStatus::Dissolved(chrono::Utc::now()),
                incorporation_date: None,
            }),
        )
        .await;

        // officer-directs-edges (Derived): a Directs edge touching the officer above.
        seed_edge_event(
            db,
            "edge-directs-dr1",
            "director:jane-officer",
            "GB-COH:DR-DISSOLVED-1",
            &EdgeWeight::Directs {
                meta: edge_meta(),
                data: DirectsData {
                    details: String::new(),
                    officer_role: None,
                },
            },
        )
        .await;

        // awardedto-edges.
        seed_edge_event(
            db,
            "edge-awardedto-dr1",
            "GB-CF:DR-CONTRACT-1",
            "GB-COH:DR-DISSOLVED-1",
            &EdgeWeight::AwardedTo {
                meta: edge_meta(),
                data: AwardedToData {
                    details: String::new(),
                },
            },
        )
        .await;

        // political-actor-nodes.
        seed_node_event(
            db,
            "political_actor:jane-councillor",
            &NodeWeight::PoliticalActor(PoliticalActorData {
                name: "Jane Councillor".into(),
                party: None,
                council_name: None,
            }),
        )
        .await;

        // political-actor-edges: an EdgeAdded row whose src_id starts with "political_actor:".
        seed_edge_event(
            db,
            "edge-pa-dr1",
            "political_actor:jane-councillor",
            "GB-COH:DR-DISSOLVED-1",
            &EdgeWeight::Directs {
                meta: edge_meta(),
                data: DirectsData {
                    details: String::new(),
                    officer_role: None,
                },
            },
        )
        .await;

        // location-nodes.
        seed_node_event(
            db,
            "GB-LOC:DR-1",
            &NodeWeight::Location(LocationData {
                name: "Site A".into(),
                postcode: Some("EC1A 1BB".into()),
                address_line_1: Some("10 Downing Street".into()),
                locality: None,
            }),
        )
        .await;

        // locatedat-edges.
        seed_edge_event(
            db,
            "edge-locatedat-dr1",
            "GB-COH:DR-DISSOLVED-1",
            "GB-LOC:DR-1",
            &EdgeWeight::LocatedAt {
                meta: edge_meta(),
                data: LocatedAtData {
                    details: String::new(),
                },
            },
        )
        .await;
    }

    #[tokio::test]
    async fn detector_readiness_all_terms_present_is_clear_no_degraded() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: detector_readiness_all_terms_present_is_clear_no_degraded (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        seed_all_terms_present(&db).await;

        let probe = DetectorReadiness;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "all 9 terms present must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            !finding
                .evidence
                .iter()
                .any(|row| row.text.to_lowercase().contains("degraded terms")),
            "a single Dissolved row (rows_examined == 1) must never trip the degraded \
             annotation, got: {:?}",
            finding.evidence
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // The one probe in this plan whose population is a fixed structural constant rather
        // than seeded data: `n_terms = term_counts.len()`, the distinct term keys across
        // MOTIF_GROUPS (9 today). Pinning the literal is what makes this non-vacuous -- a
        // `population == capable` check alone would pass with both silently 0. If a term is
        // added to MOTIF_GROUPS this assertion fails loudly, which is the intended signal.
        assert_eq!(
            coverage.population(),
            9,
            "population must equal the distinct MOTIF_GROUPS term count (9)"
        );
        assert_eq!(
            coverage.capable(),
            9,
            "capable must equal population -- this probe sets both to n_terms"
        );
    }

    #[tokio::test]
    async fn detector_readiness_evidence_names_tier_b_run_state_when_marker_present() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: detector_readiness_evidence_names_tier_b_run_state_when_marker_present \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "tier_b_marker_present",
        )
        .await;
        let mut st = Strategy::default();

        seed_all_terms_present(&db).await;
        // A legacy pre-46-04 `{ last_run_at, motif_count }`-shaped row (no `run_id`) — this
        // test's own name is `..._marker_present`, i.e. it proves the transitional `LegacyOnly`
        // state reads as "has run", not the healthy `Recorded` state.
        db.query(
            "UPSERT detector_runs:current SET last_run_at = $last_run_at, motif_count = \
             $motif_count",
        )
        .bind(("last_run_at", chrono::Utc::now().timestamp_millis()))
        .bind(("motif_count", 3i64))
        .await
        .expect("seed legacy detector_runs row")
        .check()
        .expect("seed legacy detector_runs row (check)");

        let probe = DetectorReadiness;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "detector-readiness verdict must be unaffected by the marker's presence, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains("Tier-B motif detector has run")),
            "evidence must name Tier-B's confirmed run state, got: {:?}",
            finding.evidence
        );
    }

    #[tokio::test]
    async fn detector_readiness_evidence_names_tier_b_never_run_when_marker_absent() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: detector_readiness_evidence_names_tier_b_never_run_when_marker_absent \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "tier_b_marker_absent",
        )
        .await;
        let mut st = Strategy::default();

        seed_all_terms_present(&db).await;
        // Deliberately no detector_runs:current marker seeded.

        let probe = DetectorReadiness;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "the absent marker must not gate detector-readiness's own verdict, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains("Tier-B motif detector has never run")),
            "evidence must name Tier-B's never-run state, got: {:?}",
            finding.evidence
        );
    }

    #[tokio::test]
    async fn detector_readiness_zero_political_actor_nodes_names_nepotism_and_term() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: detector_readiness_zero_political_actor_nodes_names_nepotism_and_term (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        seed_all_terms_present(&db).await;
        // Undo the PoliticalActor node + edge this fixture would otherwise have seeded — easiest
        // is a fresh namespace with everything EXCEPT political-actor-nodes/edges, so build that
        // directly instead of subtracting from the full set.
        // (seed_all_terms_present already ran above in this namespace; a second connect scoped
        // to a distinct db name keeps this fixture isolated.)
        let db2 = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "no_political_actor",
        )
        .await;

        seed_node_event(
            &db2,
            "director:jane-officer",
            &NodeWeight::Officer(OfficerData {
                name: "Jane Officer".into(),
                officer_role: Some("director".into()),
                appointed_on: None,
                resigned_on: None,
                company_number: None,
            }),
        )
        .await;
        seed_node_event(
            &db2,
            "GB-COH:DR-DISSOLVED-2",
            &NodeWeight::Entity(EntityData {
                name: "Dissolved Co 2".into(),
                company_number: Some("DR000002".into()),
                status: EntityStatus::Dissolved(chrono::Utc::now()),
                incorporation_date: None,
            }),
        )
        .await;
        seed_edge_event(
            &db2,
            "edge-directs-dr2",
            "director:jane-officer",
            "GB-COH:DR-DISSOLVED-2",
            &EdgeWeight::Directs {
                meta: edge_meta(),
                data: DirectsData {
                    details: String::new(),
                    officer_role: None,
                },
            },
        )
        .await;
        seed_edge_event(
            &db2,
            "edge-awardedto-dr2",
            "GB-CF:DR-CONTRACT-2",
            "GB-COH:DR-DISSOLVED-2",
            &EdgeWeight::AwardedTo {
                meta: edge_meta(),
                data: AwardedToData {
                    details: String::new(),
                },
            },
        )
        .await;
        seed_node_event(
            &db2,
            "GB-LOC:DR-2",
            &NodeWeight::Location(LocationData {
                name: "Site B".into(),
                postcode: Some("SW1A 1AA".into()),
                address_line_1: Some("20 Baker Street".into()),
                locality: None,
            }),
        )
        .await;
        seed_edge_event(
            &db2,
            "edge-locatedat-dr2",
            "GB-COH:DR-DISSOLVED-2",
            "GB-LOC:DR-2",
            &EdgeWeight::LocatedAt {
                meta: edge_meta(),
                data: LocatedAtData {
                    details: String::new(),
                },
            },
        )
        .await;
        // Deliberately NO PoliticalActor node and NO political_actor:-prefixed edge in db2.

        let probe = DetectorReadiness;
        let finding = probe.run(&db2, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero PoliticalActor nodes must block Nepotism bridge, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("Nepotism bridge"),
            "summary must name the specific blocked motif, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains("PoliticalActor"),
            "summary must name the specific missing term, not a bare count, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn detector_readiness_shared_dissolution_date_is_degraded_full_scan() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: detector_readiness_shared_dissolution_date_is_degraded_full_scan (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "degraded_dissolution",
        )
        .await;
        let mut st = Strategy::default();

        // 25 Dissolved rows, all sharing one date — exceeds Python's old sample-scale entirely,
        // proving D2's full scan (no sample_limit) actually examined all of them.
        const N: usize = 25;
        let shared_date = chrono::DateTime::parse_from_rfc3339("2024-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for i in 0..N {
            seed_node_event(
                &db,
                &format!("GB-COH:DR-DEGRADED-{i}"),
                &NodeWeight::Entity(EntityData {
                    name: format!("Dissolved Co {i}"),
                    company_number: Some(format!("DR{i:06}")),
                    status: EntityStatus::Dissolved(shared_date),
                    incorporation_date: None,
                }),
            )
            .await;
        }

        let probe = DetectorReadiness;
        let finding = probe.run(&db, &mut st).await;

        let degraded_row = finding
            .evidence
            .iter()
            .find(|row| row.text.to_lowercase().contains("degraded terms"));
        assert!(
            degraded_row.is_some(),
            "shared dissolution date across {N} rows must trip the degraded annotation, \
             got: {:?}",
            finding.evidence
        );
        assert!(
            degraded_row.unwrap().text.contains(&N.to_string()),
            "evidence must state the FULL rows-examined count ({N}), not a smaller sampled \
             figure, got: {}",
            degraded_row.unwrap().text
        );
    }

    /// Cross-cutting DB-gated test (Phase 46, D-05): drives all THREE `TierBRunSignal` states
    /// plus `precomputed-alerts`'s boolean-only state, each against its own `TestNs` instance,
    /// so every branch's assertion can independently fail. No single assertion here is
    /// satisfiable by more than one of the three states.
    #[tokio::test]
    async fn tier_b_run_signal_distinguishes_never_legacy_and_recorded_states() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: tier_b_run_signal_distinguishes_never_legacy_and_recorded_states (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let mut st = Strategy::default();

        // ── State 1: genuinely empty detector_runs table ────────────────────────────────────
        let db_empty = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "signal_empty",
        )
        .await;

        let empty_signal = compute_tier_b_run_signal(&db_empty)
            .await
            .expect("signal read must not error");
        assert_eq!(
            empty_signal,
            TierBRunSignal::Never,
            "an empty table must compose to Never"
        );
        let empty_line = tier_b_marker_line(Ok(empty_signal));
        assert!(empty_line.contains("never run"), "got: {empty_line}");

        let finding_empty = crate::precomputed_alerts::PrecomputedAlerts
            .run(&db_empty, &mut st)
            .await;
        assert!(
            finding_empty
                .summary
                .contains("The detector_runs table is empty"),
            "precomputed-alerts must read never-run wording against an empty table, got: {}",
            finding_empty.summary
        );

        // ── State 2: ONLY a legacy { last_run_at, motif_count }-shaped row (no run_id) ──────
        let db_legacy = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "signal_legacy",
        )
        .await;
        db_legacy
            .query("CREATE detector_runs CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "last_run_at": chrono::Utc::now().timestamp_millis(),
                    "motif_count": 7i64,
                }),
            ))
            .await
            .expect("seed legacy detector_runs row")
            .check()
            .expect("seed legacy detector_runs row (check)");

        let legacy_signal = compute_tier_b_run_signal(&db_legacy)
            .await
            .expect("signal read must not error");
        assert_eq!(
            legacy_signal,
            TierBRunSignal::LegacyOnly,
            "a legacy-only table must compose to LegacyOnly, not Recorded or Never"
        );
        let legacy_line = tier_b_marker_line(Ok(legacy_signal));
        assert!(
            legacy_line.contains("pre-migration"),
            "LegacyOnly wording must name itself as transitional, got: {legacy_line}"
        );
        assert!(
            !legacy_line.contains("started_at_ms="),
            "LegacyOnly wording must not read like Recorded's per-run detail, got: {legacy_line}"
        );
        assert!(
            !legacy_line.contains("never run"),
            "LegacyOnly wording must not read like Never, got: {legacy_line}"
        );

        let finding_legacy = crate::precomputed_alerts::PrecomputedAlerts
            .run(&db_legacy, &mut st)
            .await;
        assert!(
            finding_legacy
                .summary
                .contains("the detector_runs table shows Tier-B has run at least once"),
            "precomputed-alerts must read has-run-at-least-once wording against a legacy-only \
             table, got: {}",
            finding_legacy.summary
        );

        // ── State 3: at least one well-formed pipeline == "core" history row ────────────────
        let db_recorded = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier1",
            "signal_recorded",
        )
        .await;
        detector_runs::record_detector_run_history(
            &db_recorded,
            detector_runs::DetectorRunRecord {
                run_id: "signal-test-run".to_string(),
                started_at_ms: 1_700_000_123_000,
                pipeline: "core".to_string(),
                motif_types: vec!["PhoenixLoop".to_string()],
                motif_count: 9,
                imd_loaded: Some(true),
                imd_csv_path: Some("data/wimd_2019_wards.csv".to_string()),
                imd_denominator_declared: Some(true),
            },
        )
        .await
        .expect("seed well-formed detector_runs history row");

        let recorded_signal = compute_tier_b_run_signal(&db_recorded)
            .await
            .expect("signal read must not error");
        assert_eq!(
            recorded_signal,
            TierBRunSignal::Recorded {
                started_at_ms: 1_700_000_123_000,
                motif_count: 9,
            },
            "a well-formed core-pipeline row must compose to Recorded with its exact detail, \
             not LegacyOnly or Never"
        );
        let recorded_line = tier_b_marker_line(Ok(recorded_signal));
        assert!(
            recorded_line.contains("1700000123000"),
            "Recorded wording must quote the exact started_at_ms, got: {recorded_line}"
        );
        assert!(
            recorded_line.contains("motif_count=9"),
            "Recorded wording must quote the exact motif_count, got: {recorded_line}"
        );
        assert!(
            !recorded_line.contains("pre-migration"),
            "Recorded wording must not read like LegacyOnly, got: {recorded_line}"
        );
        assert!(
            !recorded_line.contains("never run"),
            "Recorded wording must not read like Never, got: {recorded_line}"
        );
    }
}
