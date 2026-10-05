//! Cessation-date backfill: give Phoenix a correctly-anchored failed counterparty.
//!
//! Phoenix windows on `|award_date − dissolved_dt| ≤ 180 days`
//! (`detector/src/motif/phoenix.rs`), so a dissolution date that is really an *observation* date
//! would anchor the window in the wrong place and the motif could never match. This backfill
//! exists to settle whether that is happening, by asking Companies House.
//!
//! ## The answer, measured exhaustively on 2026-08-18
//!
//! **It was not happening.** All 14,138 `GB-COH:` entities in `an_anglesey_aebaa785` were checked
//! against the CH REST API — every one, not a sample. Of the 1,966 whose dissolution date equals
//! the day their own event was written, Companies House confirms **1,966 of 1,966**: dissolved,
//! with `date_of_cessation` exactly equal to the date already held. Zero corrections were needed.
//!
//! The coincidence is real rather than suspicious: `2026-07-28` was a national bulk-dissolution
//! batch of 14,974 companies, and the company-profile stream reported those dissolutions the same
//! day they happened. Same-day delivery makes the observation date and the true cessation date
//! identical, which is the stream working, not failing.
//!
//! So [`CessationNeed::StampedDate`] is a **suspicion**, not a defect finding. It selects
//! entities whose date cannot be distinguished from an observation stamp by inspection alone, so
//! that CH can adjudicate. On this corpus CH cleared all of them.
//!
//! ## Only `GB-COH:` entities count
//!
//! Counting every dissolved entity in the graph gives 25,180 and includes 22,670 `GB-MUT:` rows —
//! the FCA Mutuals register — which carry dissolution dates from that register but have **no
//! officer edges at all**, so they sit on no Phoenix path. Fetching them would spend CH quota on
//! company numbers Companies House has never heard of. Never build this target set without the
//! prefix filter.

use urban_integrity_schema::types::node::EntityStatus;

/// The `EntityId` prefix Phoenix's motif can traverse. See the module note.
pub const CH_PREFIX: &str = "GB-COH:";

/// Why an entity does or does not need its cessation date fetched from Companies House.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CessationNeed {
    /// The graph calls it ceased, but the date equals the day its own event was written, so the
    /// date is an observation stamp rather than anything Companies House said.
    StampedDate,
    /// The graph calls it active. The company-profile stream only emits companies that *change*,
    /// so a company dissolved before subscription still reads `Active` here and CH must be asked.
    CoverageCheck,
    /// Already carries a date that cannot have been stamped at write time.
    None,
}

/// Decide what, if anything, this entity needs.
///
/// `event_day` is the `YYYY-MM-DD` of the timestamp on the event carrying `status` — not today.
///
/// Comparing against the event's own write day is the only rule that survives contact with the
/// data. The obvious alternative — blacklist `2026-07-28`, since 2,426 entities share it — is
/// wrong: that day was a genuine national bulk-dissolution batch of 14,974 companies, so the
/// blacklist mislabels 491 correct entities.
///
/// What this returns is a **candidate for verification**, not a verdict. A date equal to its own
/// write day may be a stamp or may be a same-day dissolution faithfully reported by the stream;
/// only Companies House can tell them apart. Measured 2026-08-18, CH cleared all 1,966
/// candidates on this corpus.
pub fn classify(entity_id: &str, status: &EntityStatus, event_day: &str) -> CessationNeed {
    if !entity_id.starts_with(CH_PREFIX) {
        return CessationNeed::None;
    }
    match status {
        EntityStatus::Active => CessationNeed::CoverageCheck,
        EntityStatus::Dissolved(dt) | EntityStatus::InLiquidation(dt) => {
            if dt.format("%Y-%m-%d").to_string() == event_day {
                CessationNeed::StampedDate
            } else {
                CessationNeed::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn dt(s: &str) -> DateTime<Utc> {
        format!("{s}T00:00:00Z").parse().expect("date")
    }

    /// The `GB-MUT:` lesson, pinned. 22,670 Mutuals-register entities are dissolved with real
    /// dates and no officer edges; letting them into the target set both wastes CH quota and
    /// makes the coverage percentage look 4x better than it is.
    #[test]
    fn only_companies_house_entities_are_targets() {
        for id in [
            "GB-MUT:22360R",
            "CF-PARTY:GB-CFS-100513",
            "director:smith-john",
        ] {
            assert_eq!(
                classify(id, &EntityStatus::Active, "2026-08-18"),
                CessationNeed::None,
                "{id} is not on Phoenix's traversal path and must never be fetched"
            );
        }
    }

    #[test]
    fn a_date_equal_to_its_own_write_day_is_a_stamp() {
        let s = EntityStatus::Dissolved(dt("2026-07-28"));
        assert_eq!(
            classify("GB-COH:99000042", &s, "2026-07-28"),
            CessationNeed::StampedDate
        );
    }

    /// The case that makes the write-day rule necessary rather than merely convenient.
    ///
    /// `2026-07-28` was a real national bulk-dissolution batch. An entity whose event was
    /// written on 08-11 and still says 07-28 got that date from Companies House, and a rule
    /// that blacklists the date itself would wrongly re-fetch 491 correct entities.
    #[test]
    fn a_bulk_batch_date_written_later_is_real_not_a_stamp() {
        let s = EntityStatus::Dissolved(dt("2026-07-28"));
        assert_eq!(
            classify("GB-COH:02399140", &s, "2026-08-11"),
            CessationNeed::None
        );
    }

    #[test]
    fn a_historical_date_is_real() {
        let s = EntityStatus::Dissolved(dt("2023-04-16"));
        assert_eq!(
            classify("GB-COH:06495696", &s, "2026-08-11"),
            CessationNeed::None
        );
    }

    /// Pre-subscription dissolutions still read `Active`, which is exactly the coverage half the
    /// stream structurally cannot supply.
    #[test]
    fn an_active_company_is_a_coverage_check() {
        assert_eq!(
            classify("GB-COH:12345678", &EntityStatus::Active, "2026-08-18"),
            CessationNeed::CoverageCheck
        );
    }

    /// `InLiquidation` anchors the window the same way `Dissolved` does, so it is subject to the
    /// same stamp test rather than being waved through.
    #[test]
    fn in_liquidation_is_stamp_checked_too() {
        let s = EntityStatus::InLiquidation(dt("2026-08-04"));
        assert_eq!(
            classify("GB-COH:12963211", &s, "2026-08-04"),
            CessationNeed::StampedDate
        );
        let s = EntityStatus::InLiquidation(dt("2025-01-09"));
        assert_eq!(
            classify("GB-COH:12963211", &s, "2026-08-04"),
            CessationNeed::None
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Fetch cache
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// One cached Companies House company-profile reply.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedProfile {
    pub company_number: String,
    /// CH `company_status`, lowercased (`active`, `dissolved`, `liquidation`, …).
    #[serde(default)]
    pub company_status: Option<String>,
    /// CH `date_of_cessation`, `YYYY-MM-DD`. Absent for a company that has not ceased.
    #[serde(default)]
    pub date_of_cessation: Option<String>,
    /// CH returned 404 — the company number is not known to Companies House.
    #[serde(default)]
    pub gone: bool,
}

/// The CH statuses that mean "this company has ceased trading" for Phoenix's purposes.
///
/// Phoenix's counterparty term is a *failed* company, so receivership and administration count
/// alongside dissolution and liquidation.
pub const CEASED_STATUSES: &[&str] =
    &["dissolved", "liquidation", "receivership", "administration"];

impl CachedProfile {
    pub fn has_ceased(&self) -> bool {
        self.company_status
            .as_deref()
            .is_some_and(|s| CEASED_STATUSES.contains(&s))
    }
}

/// Read a JSONL profile cache, tolerating the shape written by
/// `scripts/ch_cessation_backfill.py` so the ~12,500 profiles already fetched are not re-paid.
///
/// The Python cache nests the profile under `p` and marks 404s with `_gone`; rows in the native
/// shape are read directly. A malformed line is skipped rather than aborting the load — the cache
/// is an optimisation, and a truncated final line from an interrupted run must not block a resume.
pub fn parse_cache(contents: &str) -> Vec<CachedProfile> {
    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // Native shape.
        if v.get("company_number").is_some() {
            if let Ok(p) = serde_json::from_value::<CachedProfile>(v.clone()) {
                out.push(p);
                continue;
            }
        }
        // `scripts/ch_cessation_backfill.py` shape: {"cn": "...", "p": {...}}
        let (Some(cn), Some(p)) = (v.get("cn").and_then(|c| c.as_str()), v.get("p")) else {
            continue;
        };
        out.push(CachedProfile {
            company_number: cn.to_string(),
            company_status: p
                .get("company_status")
                .and_then(|s| s.as_str())
                .map(|s| s.to_ascii_lowercase()),
            date_of_cessation: p
                .get("date_of_cessation")
                .and_then(|s| s.as_str())
                .map(|s| s[..s.len().min(10)].to_string()),
            gone: p.get("_gone").and_then(|g| g.as_bool()).unwrap_or(false),
        });
    }
    out
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    /// The Python cache is 12,529 profiles deep, 514 of them carrying a real cessation date.
    /// Failing to read it would re-spend ~2 hours of rate-limited CH quota.
    #[test]
    fn reads_the_python_scripts_cache_shape() {
        let jsonl = r#"
{"cn":"99000042","p":{"company_status":"dissolved","date_of_cessation":"2023-04-16","company_name":"X"}}
{"cn":"0","p":{"company_status":null,"date_of_cessation":null,"_gone":true}}
{"cn":"12345678","p":{"company_status":"active","date_of_cessation":null}}
"#;
        let got = parse_cache(jsonl);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].company_number, "99000042");
        assert_eq!(got[0].date_of_cessation.as_deref(), Some("2023-04-16"));
        assert!(got[0].has_ceased());
        assert!(got[1].gone, "a 404 must survive the round trip as `gone`");
        assert!(!got[2].has_ceased());
    }

    /// An interrupted fetch leaves a half-written final line. The cache is an optimisation, so a
    /// bad line must be skipped, never fatal — otherwise one crash makes every later resume fail.
    #[test]
    fn a_truncated_final_line_does_not_abort_the_load() {
        let jsonl =
            "{\"cn\":\"99000042\",\"p\":{\"company_status\":\"dissolved\"}}\n{\"cn\":\"0028";
        let got = parse_cache(jsonl);
        assert_eq!(got.len(), 1, "the good line must still load");
    }

    #[test]
    fn receivership_and_administration_count_as_ceased() {
        for s in ["dissolved", "liquidation", "receivership", "administration"] {
            let p = CachedProfile {
                company_number: "1".into(),
                company_status: Some(s.into()),
                date_of_cessation: None,
                gone: false,
            };
            assert!(p.has_ceased(), "{s} must count as a failed counterparty");
        }
        let active = CachedProfile {
            company_number: "1".into(),
            company_status: Some("active".into()),
            date_of_cessation: None,
            gone: false,
        };
        assert!(!active.has_ceased());
    }
}

#[cfg(test)]
mod live_cache_tests {
    use super::*;

    /// Parse the real cache, not a fixture. Fixtures prove the parser reads what the test author
    /// imagined the file looks like; this proves it reads the file that actually exists.
    ///
    /// Asserts invariants, not a row count. `backfill cessation` APPENDS to this very file, so a
    /// pinned count fails the moment the tool does its job — this test did exactly that, going
    /// from 12,529 to 13,002 mid-run. What must hold is that **every** non-empty line parses: a
    /// parser regression shows up as silently dropped rows, which is the failure mode that
    /// matters and the one a count would mask anyway.
    #[test]
    // Reads the operator's local cache, which `.gitignore` excludes — so this cannot run
    // on a fresh clone or in CI. It is `#[ignore]`d rather than skipped at runtime so the
    // default `cargo test` summary reports it as *ignored* instead of *passed*: until
    // 2026-08-20 an absent file returned early and the test counted as green, which is the
    // silent-pass failure mode this suite exists to catch. Run it with:
    //     cargo test -p urban-integrity-ingestor -- --ignored the_real_python_cache_parses_without_dropping_a_line
    #[ignore = "needs the operator's local data/ch-company-status-cache.jsonl; run with --ignored"]
    fn the_real_python_cache_parses_without_dropping_a_line() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/ch-company-status-cache.jsonl"
        );
        // Hard failure, not a skip: this test is `#[ignore]`d by default, so reaching this
        // line means someone asked for it explicitly and is owed a real answer.
        let contents = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "{path} is missing ({e}). This test asserts invariants against the real \
                 Companies House corpus, not a fixture — populate it with `backfill cessation` \
                 and re-run. Do not commit the file: it carries real names and dates of birth."
            )
        });
        let lines = contents.lines().filter(|l| !l.trim().is_empty()).count();
        let profiles = parse_cache(&contents);
        assert_eq!(
            profiles.len(),
            lines,
            "every non-empty line must parse — a dropped row is a silent data loss"
        );
        // The cache only ever grows, so these are floors, not equalities.
        assert!(profiles.len() >= 12_529, "cache shrank: {}", profiles.len());
        assert!(
            profiles
                .iter()
                .filter(|p| p.date_of_cessation.is_some())
                .count()
                >= 514,
            "real cessation dates must not disappear"
        );
        assert!(
            profiles.iter().any(|p| p.gone),
            "the 404 marker must survive the round trip"
        );
        assert!(
            profiles.iter().any(|p| p.has_ceased()),
            "ceased companies must be recognised"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Runner
// ─────────────────────────────────────────────────────────────────────────────────────────────

use crate::backfill::cursor::{resume_index, CursorState, ResumeCursor};
use crate::backfill::session::DbSession;
use crate::backfill::{extract_id_str, BackfillStats};
use anyhow::Result;
use urban_integrity_schema::anomaly::{
    AnomalySeverity, AnomalyStatus, DataAnomaly, KIND_MISSING_REQUIRED_FIELD,
};
use urban_integrity_schema::types::node::{EntityData, NodeWeight};

/// Cursor job name — `backfill_cursors:cessation`.
pub const CESSATION_JOB: &str = "cessation";

/// One entity that needs a cessation date, in the order the run will process it.
#[derive(Debug, Clone, PartialEq)]
pub struct CessationTarget {
    pub entity_id: String,
    pub company_number: String,
    pub need: CessationNeed,
    pub entity: EntityData,
}

/// Resolve the latest status event per entity, then classify each one.
///
/// A `NodeUpdated` supersedes the `NodeAdded` it follows, so the target set must be built from
/// the *latest* event per entity — classifying raw rows would re-target every entity the previous
/// run already corrected, and the run would never converge.
pub fn select_targets(rows: Vec<serde_json::Value>) -> Vec<CessationTarget> {
    select_targets_reporting_drops(rows).0
}

/// As [`select_targets`], but also returns every row it had to drop and why.
///
/// A silent drop is how this project loses data. The caller is expected to count these, record a
/// bounded sample as `missing_required_field` anomalies, and refuse to describe the run as clean
/// while any are outstanding.
pub fn select_targets_reporting_drops(
    rows: Vec<serde_json::Value>,
) -> (Vec<CessationTarget>, Vec<(String, String)>) {
    use std::collections::HashMap;

    let mut dropped: Vec<(String, String)> = Vec::new();

    // entity_id -> (timestamp, EntityData)
    let mut latest: HashMap<String, (u64, EntityData)> = HashMap::new();
    for row in rows {
        let Some(entity_id) = extract_id_str(&row["payload"]["entity_id"]) else {
            continue;
        };
        if !entity_id.starts_with(CH_PREFIX) {
            continue;
        }
        let Some(wj) = row["payload"]["weight_json"].as_str() else {
            continue;
        };
        let Ok(NodeWeight::Entity(e)) = serde_json::from_str::<NodeWeight>(wj) else {
            continue;
        };
        // A missing timestamp must not silently become the epoch. `unwrap_or(0)` here made
        // `event_day` 1970-01-01 for every row, which never equals a cessation date, so the
        // classifier reported 2 stamped entities where the graph holds 1,966 — a wrong answer
        // that logged nothing.
        //
        // `require_u64` names the field and lists the keys the row DID carry, so the next time a
        // query stops selecting a column it is an error at that moment rather than a plausible
        // number found months later. Drops are counted and returned, never absorbed.
        let ts = match crate::backfill::require::require_u64(&row, "timestamp") {
            Ok(ts) => ts,
            Err(e) => {
                dropped.push((entity_id.clone(), e.to_string()));
                continue;
            }
        };
        match latest.get(&entity_id) {
            Some((prev, _)) if *prev >= ts => {}
            _ => {
                latest.insert(entity_id, (ts, e));
            }
        }
    }

    let mut out: Vec<CessationTarget> = latest
        .into_iter()
        .filter_map(|(entity_id, (ts, entity))| {
            let event_day = chrono::DateTime::from_timestamp_millis(ts as i64)?
                .format("%Y-%m-%d")
                .to_string();
            let need = classify(&entity_id, &entity.status, &event_day);
            if need == CessationNeed::None {
                return None;
            }
            let company_number = entity
                .company_number
                .clone()
                .unwrap_or_else(|| entity_id.trim_start_matches(CH_PREFIX).to_string());
            Some(CessationTarget {
                entity_id,
                company_number,
                need,
                entity,
            })
        })
        .collect();

    // Stamped dates first: those are entities Phoenix already treats as counterparties but
    // windows in the wrong place, so correcting them changes detector behaviour immediately.
    // Coverage checks only ever *add* counterparties. Ordering is also the cursor's resume key,
    // so it must be deterministic — hence the tie-break on entity_id.
    out.sort_by(|a, b| match (a.need, b.need) {
        (CessationNeed::StampedDate, CessationNeed::CoverageCheck) => std::cmp::Ordering::Less,
        (CessationNeed::CoverageCheck, CessationNeed::StampedDate) => std::cmp::Ordering::Greater,
        _ => a.entity_id.cmp(&b.entity_id),
    });
    (out, dropped)
}

/// Apply a fetched profile to an entity, returning the corrected weight if anything changed.
///
/// **Fidelity rule:** the new weight is the existing weight with only `status` replaced. Dropping
/// fields here would repeat the `projector.rs` defect that cost Phase 29 two false root causes —
/// 30,836 events carried `application_received_date` while the projected table had zero, because
/// a `json!` literal silently omitted the field.
pub fn apply_profile(entity: &EntityData, profile: &CachedProfile) -> Option<EntityData> {
    if !profile.has_ceased() {
        return None;
    }
    let date = profile.date_of_cessation.as_deref()?;
    let parsed: chrono::DateTime<chrono::Utc> = format!("{date}T00:00:00Z").parse().ok()?;
    let status = match profile.company_status.as_deref() {
        Some("dissolved") => urban_integrity_schema::types::node::EntityStatus::Dissolved(parsed),
        _ => urban_integrity_schema::types::node::EntityStatus::InLiquidation(parsed),
    };
    if status == entity.status {
        return None;
    }
    let mut updated = entity.clone();
    updated.status = status;
    Some(updated)
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn dt(s: &str) -> DateTime<Utc> {
        format!("{s}T00:00:00Z").parse().expect("date")
    }

    fn row(
        entity_id: &str,
        ts: u64,
        status: urban_integrity_schema::types::node::EntityStatus,
    ) -> serde_json::Value {
        let e = EntityData {
            name: "ACME LIMITED".into(),
            company_number: Some(entity_id.trim_start_matches(CH_PREFIX).into()),
            status,
            incorporation_date: Some("1990-01-01".parse().unwrap()),
        };
        serde_json::json!({
            "timestamp": ts,
            "payload": {
                "entity_id": entity_id,
                "weight_json": serde_json::to_string(&NodeWeight::Entity(e)).unwrap(),
            }
        })
    }

    const D0728: u64 = 1_785_196_800_000; // 2026-07-28T00:00:00Z
    const D0811: u64 = 1_786_492_800_000; // 2026-08-11T00:00:00Z

    /// **The convergence property.** A `NodeUpdated` supersedes the `NodeAdded` it follows, so a
    /// run must classify the latest event per entity. Selecting over raw rows would re-target
    /// every entity the previous run already corrected and the backfill would never finish.
    #[test]
    fn a_later_correction_removes_the_entity_from_the_target_set() {
        let rows = vec![
            // Original stamped row: dissolved 07-28, written 07-28.
            row(
                "GB-COH:99000042",
                D0728,
                urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2026-07-28")),
            ),
            // The correction: real date, written later.
            row(
                "GB-COH:99000042",
                D0811,
                urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2023-04-16")),
            ),
        ];
        assert!(
            select_targets(rows).is_empty(),
            "an entity already corrected must not be re-targeted"
        );
    }

    #[test]
    fn an_uncorrected_stamp_is_still_targeted() {
        let rows = vec![row(
            "GB-COH:99000042",
            D0728,
            urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2026-07-28")),
        )];
        let got = select_targets(rows);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].need, CessationNeed::StampedDate);
        assert_eq!(got[0].company_number, "99000042");
    }

    /// Stamped dates outrank coverage checks: correcting them changes detector behaviour now,
    /// whereas a coverage check only ever adds a counterparty. The order is also the cursor's
    /// resume key, so it must be deterministic.
    #[test]
    fn stamped_dates_are_processed_before_coverage_checks() {
        let rows = vec![
            row(
                "GB-COH:11111111",
                D0728,
                urban_integrity_schema::types::node::EntityStatus::Active,
            ),
            row(
                "GB-COH:22222222",
                D0728,
                urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2026-07-28")),
            ),
        ];
        let got = select_targets(rows);
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].need,
            CessationNeed::StampedDate,
            "stamped must sort first"
        );
        assert_eq!(got[1].need, CessationNeed::CoverageCheck);
    }

    /// **The timestamp regression.** `select_targets` derives the event's write day from
    /// `row["timestamp"]`, and the shared scan helper did not select that column. Every row
    /// therefore read as the epoch, `event_day` became `1970-01-01`, and since no cessation date
    /// equals that, the classifier reported **2** stamped entities where the graph holds
    /// **1,966** — a wrong answer that logged nothing and passed every unit test, because the
    /// fixtures all supplied a timestamp the real query did not.
    ///
    /// A row with no usable timestamp is now dropped, never guessed at.
    #[test]
    fn a_row_without_a_timestamp_is_dropped_not_treated_as_the_epoch() {
        let mut r = row(
            "GB-COH:99000042",
            D0728,
            urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2026-07-28")),
        );
        // Exactly what the scan returned before `timestamp` was added to its SELECT.
        r.as_object_mut().unwrap().remove("timestamp");
        assert!(
            select_targets(vec![r]).is_empty(),
            "a row with no timestamp must be dropped, not classified against 1970-01-01"
        );
    }

    #[test]
    fn mutuals_register_entities_never_enter_the_target_set() {
        let rows = vec![serde_json::json!({
            "timestamp": D0728,
            "payload": {
                "entity_id": "GB-MUT:22360R",
                "weight_json": serde_json::to_string(&NodeWeight::Entity(EntityData {
                    name: "ClwydAlyn Housing Limited".into(),
                    company_number: None,
                    status: urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2026-07-28")),
                    incorporation_date: None,
                })).unwrap(),
            }
        })];
        assert!(select_targets(rows).is_empty());
    }

    /// Fidelity: only `status` may change. The projector defect that cost Phase 29 two false root
    /// causes was a `json!` literal silently dropping a field it did not name.
    #[test]
    fn applying_a_profile_preserves_every_other_field() {
        let entity = EntityData {
            name: "JONAS WELLS,LIMITED".into(),
            company_number: Some("99000042".into()),
            status: urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2026-07-28")),
            incorporation_date: Some("1905-12-08".parse().unwrap()),
        };
        let profile = CachedProfile {
            company_number: "99000042".into(),
            company_status: Some("dissolved".into()),
            date_of_cessation: Some("2023-04-16".into()),
            gone: false,
        };
        let got = apply_profile(&entity, &profile).expect("a real date must produce an update");
        assert_eq!(got.name, entity.name);
        assert_eq!(got.company_number, entity.company_number);
        assert_eq!(got.incorporation_date, entity.incorporation_date);
        assert_eq!(
            got.status,
            urban_integrity_schema::types::node::EntityStatus::Dissolved(dt("2023-04-16"))
        );
    }

    /// A company CH still calls active, and one that ceased but has no date, must both be
    /// no-ops — writing a status with no usable date just re-creates the defect.
    #[test]
    fn a_profile_with_nothing_to_add_produces_no_update() {
        let entity = EntityData {
            name: "ACME".into(),
            company_number: None,
            status: urban_integrity_schema::types::node::EntityStatus::Active,
            incorporation_date: None,
        };
        let active = CachedProfile {
            company_number: "1".into(),
            company_status: Some("active".into()),
            date_of_cessation: None,
            gone: false,
        };
        assert!(apply_profile(&entity, &active).is_none());

        let dateless = CachedProfile {
            company_number: "1".into(),
            company_status: Some("dissolved".into()),
            date_of_cessation: None,
            gone: false,
        };
        assert!(
            apply_profile(&entity, &dateless).is_none(),
            "a ceased status with no date must not be written — that is the defect, not the fix"
        );
    }
}

/// Fetches real cessation dates from Companies House and writes them back as `NodeUpdated`.
pub struct CessationBackfiller {
    session: DbSession,
    api_key: String,
    rate_limit: u32,
    cache_path: std::path::PathBuf,
    pub dry_run: bool,
    /// Ignore (and clear) any saved cursor, walking the whole candidate list again.
    pub restart: bool,
    /// Stop after this many *network* fetches. `None` for no bound.
    pub max_fetches: Option<usize>,
}

impl CessationBackfiller {
    pub fn new(session: DbSession, api_key: String, rate_limit: u32, dry_run: bool) -> Self {
        Self {
            session,
            api_key,
            rate_limit,
            cache_path: std::path::PathBuf::from("data/ch-company-status-cache.jsonl"),
            dry_run,
            restart: false,
            max_fetches: None,
        }
    }

    pub fn restart(mut self, restart: bool) -> Self {
        self.restart = restart;
        self
    }

    pub fn max_fetches(mut self, max: Option<usize>) -> Self {
        self.max_fetches = max;
        self
    }

    pub fn cache_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.cache_path = path.into();
        self
    }

    fn load_cache(&self) -> std::collections::HashMap<String, CachedProfile> {
        std::fs::read_to_string(&self.cache_path)
            .map(|c| {
                parse_cache(&c)
                    .into_iter()
                    .map(|p| (p.company_number.clone(), p))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Append one freshly fetched profile so an interrupted run never re-pays for it.
    fn append_cache(&self, profile: &CachedProfile) -> Result<()> {
        use std::io::Write;
        if let Some(dir) = self.cache_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cache_path)?;
        writeln!(f, "{}", serde_json::to_string(profile)?)?;
        Ok(())
    }

    pub async fn run(&self) -> Result<BackfillStats> {
        let mut stats = BackfillStats::default();

        // Both event kinds: a NodeUpdated supersedes the NodeAdded it follows, and reading only
        // NodeAdded would re-target every entity a previous run already corrected.
        let mut rows =
            crate::backfill::scan_events_by_type(&self.session, "NodeAdded", "\"type\":\"Entity\"")
                .await?;
        rows.extend(
            crate::backfill::scan_events_by_type(
                &self.session,
                "NodeUpdated",
                "\"type\":\"Entity\"",
            )
            .await?,
        );

        let (targets, dropped) = select_targets_reporting_drops(rows);

        // A row the classifier could not read is not "nothing happened" — it is an input silently
        // missing from the answer. Count every one, persist a bounded sample as an anomaly, and
        // say so in the summary. This is the exact class of loss that let a 2-vs-1,966 error pass
        // sixteen green unit tests.
        if !dropped.is_empty() {
            let mut counter = crate::backfill::anomaly_sink::DropCounter::default();
            tracing::warn!(
                dropped = dropped.len(),
                "Cessation backfill: rows DROPPED — a field the classifier requires was absent. \
                 These entities are missing from the target set. Recorded as \
                 `missing_required_field` anomalies."
            );
            for (entity_id, reason) in &dropped {
                if !counter.should_record() {
                    continue;
                }
                if self.dry_run {
                    continue;
                }
                crate::backfill::anomaly_sink::record_anomaly(
                    &self.session,
                    &DataAnomaly {
                        kind: KIND_MISSING_REQUIRED_FIELD.to_string(),
                        severity: AnomalySeverity::Medium,
                        subject: entity_id.clone(),
                        detail: "this entity was excluded from the cessation target set because a \
                             required field was absent from the scan result, so its status was \
                             never checked against Companies House"
                            .to_string(),
                        evidence_json: serde_json::json!({
                            "reason": reason,
                            "total_dropped_this_run": dropped.len(),
                        })
                        .to_string(),
                        source: format!("backfill:{CESSATION_JOB}"),
                        status: AnomalyStatus::Open,
                    },
                )
                .await?;
            }
            stats.errors += dropped.len();
        }

        stats.scanned = targets.len();
        let stamped = targets
            .iter()
            .filter(|t| t.need == CessationNeed::StampedDate)
            .count();
        tracing::info!(
            targets = stats.scanned,
            stamped_dates = stamped,
            coverage_checks = stats.scanned - stamped,
            "Cessation backfill: targets selected"
        );
        if targets.is_empty() {
            return Ok(stats);
        }

        let cache = self.load_cache();
        let cached_hits = targets
            .iter()
            .filter(|t| cache.contains_key(&t.company_number))
            .count();
        tracing::info!(
            cache_entries = cache.len(),
            targets_already_cached = cached_hits,
            needing_fetch = targets.len() - cached_hits,
            "Cessation backfill: cache state"
        );

        // A dry run must neither read nor write the cursor: it changes nothing, so it must not
        // claim progress either.
        let cursor = ResumeCursor::new(self.session.clone(), CESSATION_JOB);
        let mut start = 0usize;
        if !self.dry_run {
            if self.restart {
                cursor.clear().await?;
                tracing::info!(
                    "Cessation backfill: --restart given \u{2014} saved cursor discarded"
                );
            } else if let Some(state) = cursor.load().await? {
                start = resume_index(&targets, &state.last_record_id, |t| t.entity_id.as_str());
                // Totals carry across resumes so the final numbers describe the whole job, not
                // just the last leg.
                stats.matched = state.matched;
                stats.skipped = state.skipped;
                stats.errors = state.errors;
                tracing::info!(
                    resumed_after = %state.last_record_id,
                    skipping = start,
                    "Cessation backfill: resuming"
                );
            }
        }

        let rest = crate::backfill::ch::ChRestClient::new(self.api_key.clone(), self.rate_limit);
        let mut fetches = 0usize;

        for target in targets.iter().skip(start) {
            let profile = match cache.get(&target.company_number) {
                Some(p) => p.clone(),
                None => {
                    if self.dry_run {
                        stats.skipped += 1;
                        continue;
                    }
                    if self.max_fetches.is_some_and(|m| fetches >= m) {
                        tracing::info!(fetches, "Cessation backfill: --max-fetches reached");
                        break;
                    }
                    fetches += 1;
                    match rest.company_profile(&target.company_number).await {
                        Ok(Some(p)) => {
                            self.append_cache(&p)?;
                            p
                        }
                        // A 429 backed off; leave the cursor where it is and stop so the next run
                        // resumes rather than hammering a rate-limited endpoint.
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(cn = %target.company_number, error = %e, "CH profile fetch failed");
                            stats.errors += 1;
                            continue;
                        }
                    }
                }
            };

            match apply_profile(&target.entity, &profile) {
                Some(updated) => {
                    if !self.dry_run {
                        crate::backfill::upsert_node_updated_event(
                            &self.session,
                            CESSATION_JOB,
                            &target.entity_id,
                            &NodeWeight::Entity(updated),
                        )
                        .await?;
                    }
                    stats.matched += 1;
                }
                None => stats.skipped += 1,
            }

            if !self.dry_run && stats.matched % 100 == 0 {
                cursor
                    .save(&CursorState {
                        last_record_id: target.entity_id.clone(),
                        processed: stats.matched + stats.skipped,
                        matched: stats.matched,
                        skipped: stats.skipped,
                        errors: stats.errors,
                    })
                    .await?;
            }
        }

        if !self.dry_run {
            cursor.clear().await?;
        }
        tracing::info!(
            scanned = stats.scanned,
            matched = stats.matched,
            skipped = stats.skipped,
            errors = stats.errors,
            fetches,
            "Cessation backfill complete"
        );
        Ok(stats)
    }
}
