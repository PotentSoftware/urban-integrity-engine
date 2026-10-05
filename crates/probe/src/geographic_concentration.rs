//! `GeographicConcentration` Tier-1 probe — ports `probe_geographic_concentration`
//! (`scripts/analyst_probe.py:1029-1064`), closing the Severe CLEAR/DATA_GAP hazard named in
//! VRD-06/VRD-07 (the phase 32 design notes, ROADMAP success criterion 3).
//!
//! Python's own code has **no check for zero `Location` nodes**: an empty `locs` list falls
//! straight through to `repeat_streets` being empty, which returns `CLEAR` "geographically
//! dispersed" — a DATA_GAP-shaped truth (nothing was observed) rendered as a CLEAR-shaped claim
//! (nothing suspicious was found). This port fixes that: `population == 0` short-circuits to
//! `Verdict::DataGap` before any street/sector computation runs, and the fix is proven by a
//! DB-gated fixture below (`geographic_concentration_zero_locations_is_data_gap`) that fails the
//! build if this probe ever again reaches `Clear` on an empty `Location` set.
//!
//! # ADJ-01: this probe counted events, not locations
//!
//! Phase 36's differential recorded this probe at 23,322 streets carrying >= 2 applications
//! against Python's 1,697 on the same corpus and the same metric — a 13.7x gap that a
//! verdict-only comparison missed entirely, because both sides returned `WEAK`/`MEDIUM`
//! (the plan 36-05 summary). That summary flagged `street_key` normalisation as the first place to
//! look. It was not the cause: `street_key` and `postcode_sector` are verbatim ports of the
//! Python functions, with parity tests below asserting exactly that.
//!
//! The cause was the scan. This probe called `scan::scan_entities_by_type`, which yields one row
//! per `NodeAdded` *event*, while Python's `fetch_nodes` dedups latest-per-entity
//! (`analyst_probe.py:301-304`). Measured against `an_anglesey_aebaa785` on 2026-09-01:
//! 102,897 Location events over 61,756 distinct Locations, of which **22,839 carried >= 2
//! events**. Each of those crossed the repeat-street threshold on its duplicates alone, which
//! accounts for the gap (22,839 + 1,697 = 24,536 against an observed 23,322; the residual is
//! duplicated entities collapsing onto a shared street key). Python was the correct side.
//!
//! Fixed by switching to `scan_latest_entities_by_type`, the same per-entity dedup
//! `identity_integrity.rs` uses. `geographic_concentration_reingested_location_is_not_a_cluster`
//! below is the regression guard. The finding was derived independently against `graph_events`
//! via the Phase 38 population oracle, never by preferring Python's figure — the binding
//! constraint on retaining `analyst_probe.py` as an oracle rather than as ground truth.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{EvidenceRow, Finding, Severity, Verdict};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// Matches an already-uppercased, all-spaces-stripped UK postcode: outward code, inward sector
/// digit, then the two inward-code letters. Verbatim port of `postcode_sector`'s regex
/// (`scripts/analyst_probe.py:357`).
static POSTCODE_SECTOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Z]{1,2}\d{1,2}[A-Z]?)\s*(\d)[A-Z]{2}$").expect("static regex is valid")
});

/// Strips a leading house-number token (e.g. `"10 "`, `"12A "`). Verbatim port of `street_key`'s
/// first substitution (`scripts/analyst_probe.py:375`).
static STREET_LEADING_NUMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\d+[A-Za-z]?\s+").expect("static regex is valid"));

/// Best-effort strips a trailing UK-postcode-shaped suffix, if present. Verbatim port of
/// `street_key`'s second substitution (`scripts/analyst_probe.py:376`).
static STREET_TRAILING_POSTCODE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s+[a-z]{1,2}\d{1,2}[a-z]?\s*\d[a-z]{2}$").expect("static regex is valid")
});

/// `postcode_sector` — normalises to uppercase, strips all spaces, then matches the UK
/// outward+inward postcode shape, returning `"{district} {sector_digit}"`.
///
/// Verbatim port of `scripts/analyst_probe.py:354-361`.
fn postcode_sector(pc: &str) -> Option<String> {
    if pc.is_empty() {
        return None;
    }
    let normalised = pc.trim().to_uppercase().replace(' ', "");
    let caps = POSTCODE_SECTOR_RE.captures(&normalised)?;
    Some(format!("{} {}", &caps[1], &caps[2]))
}

/// `street_key` — strips a leading house-number token from the raw address, then lowercases and
/// trims, then best-effort strips a trailing UK-postcode-shaped suffix.
///
/// Verbatim port of `scripts/analyst_probe.py:374-379` (same operation order: strip-leading
/// happens on the raw-case string, strip-trailing happens after lowercasing).
fn street_key(addr: &str) -> Option<String> {
    if addr.is_empty() {
        return None;
    }
    let after_leading = STREET_LEADING_NUMBER_RE.replace(addr, "");
    let lowered = after_leading.trim().to_lowercase();
    let after_trailing = STREET_TRAILING_POSTCODE_RE.replace(&lowered, "");
    let result = after_trailing.trim();
    if result.is_empty() {
        None
    } else {
        Some(result.to_string())
    }
}

/// The Severe CLEAR/DATA_GAP hazard fix (VRD-06) — see module doc comment.
pub struct GeographicConcentration;

#[async_trait::async_trait]
impl Probe for GeographicConcentration {
    fn name(&self) -> &'static str {
        "geographic-concentration"
    }

    /// Can-name-entities, deferred: `locs`'s `(EntityId, NodeWeight)` pairs from
    /// `scan_latest_entities_by_type` (~line 128) are held, and each row's id is discarded via
    /// `for (_entity_id, weight) in &locs` (~line 183) while building the street/sector tallies.
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
        "Tier-1"
    }

    fn hypothesis(&self) -> &'static str {
        "Are planning applications geographically concentrated by street or postcode sector \
         rather than dispersed, and is the Location population non-zero?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // `scan_latest_entities_by_type`, NOT `scan_entities_by_type` — the latter returns one
        // row per *event*, so a re-ingested Location contributes once per NodeAdded row rather
        // than once per location. That was ADJ-01's defect (see this file's header): every
        // duplicated entity crossed the `>= 2` repeat-street threshold on its own duplicates,
        // manufacturing a spurious cluster from a location with exactly one real application.
        let locs = match scan::scan_latest_entities_by_type(
            db,
            "NodeAdded",
            "\"type\":\"Location\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result — Probe::run is
                // infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Location scan failed: {e}")),
                    summary: "Location scan failed — geographic clustering cannot be evaluated."
                        .to_string(),
                    cause: None,
                    // No Python analog (Python has no scan-failure branch here) — defaulted
                    // per 35-01 convention: an analog-less DataGap/Blocked branch is Medium.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("Location scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        let population = locs.len() as u64;
        if population == 0 {
            // The Severe hazard fix: never fall through to a CLEAR verdict on zero data.
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("no Location nodes in graph_events"),
                summary: "No Location nodes present — geographic clustering cannot be evaluated."
                    .to_string(),
                cause: None,
                // The Severe hazard fix (VRD-06) has no Python analog — defaulted per 35-01
                // convention: an analog-less DataGap/Blocked branch is Medium.
                severity: Severity::Medium,
                evidence: vec![EvidenceRow {
                    text: "Zero Location nodes evaluated — geographic clustering cannot be \
                           measured."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let mut streets: HashMap<String, u64> = HashMap::new();
        let mut sectors: HashMap<String, u64> = HashMap::new();
        for (_entity_id, weight) in &locs {
            let postcode = weight
                .get("postcode")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some(sector) = postcode_sector(postcode) {
                *sectors.entry(sector).or_insert(0) += 1;
            }

            // Python: `n.get("address_line_1") or n.get("name") or ""` — fall back to `name`
            // (required on every LocationData) only when address_line_1 is absent or empty.
            let address_line_1 = weight
                .get("address_line_1")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let name = weight.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let street_source = address_line_1.unwrap_or(name);
            if let Some(street) = street_key(street_source) {
                *streets.entry(street).or_insert(0) += 1;
            }
        }

        let repeat_streets: Vec<(&String, &u64)> =
            streets.iter().filter(|(_, &count)| count >= 2).collect();
        let n_repeat = repeat_streets.len() as u64;
        let n_sectors = sectors.len();

        // `capable == population` always for this probe — every Location node is, by
        // definition, capable of contributing to a street/sector concentration count; there is
        // no structural exclusion. `hit_threshold: u64::MAX` is deliberate: Python's vocabulary
        // for this probe is WEAK/CLEAR only, never HIT (`analyst_probe.py:1050-1064` has no HIT
        // branch). `u64::MAX` makes `classify`'s `matches >= hit_threshold` branch structurally
        // unreachable — do not "fix" this back to a small number.
        let verdict = Verdict::classify(population, population, n_repeat, u64::MAX, "");

        // Python: WEAK/MEDIUM when repeat_streets non-empty (`analyst_probe.py:1050-1057`),
        // CLEAR/INFO otherwise (`:1058-1064`).
        let (severity, summary, evidence_text) = if n_repeat > 0 {
            (
                Severity::Medium,
                format!(
                    "{} carry \u{2265}2 applications — coordinated-activity signal.",
                    plural(n_repeat, "street", "streets")
                ),
                format!(
                    "{} evaluated; {} carry \u{2265}2 applications across {}.",
                    plural(population, "Location node", "Location nodes"),
                    plural(n_repeat, "street", "streets"),
                    plural(n_sectors as u64, "postcode sector", "postcode sectors")
                ),
            )
        } else {
            (
                Severity::Info,
                "No notable street-level repetition; geographically dispersed.".to_string(),
                format!(
                    "{} evaluated — no notable street-level repetition.",
                    plural(population, "Location node", "Location nodes")
                ),
            )
        };

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
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
    use urban_integrity_schema::types::node::{LocationData, NodeWeight};

    async fn seed_node_event(db: &Surreal<Any>, entity_id: &str, weight: &NodeWeight) {
        seed_node_event_at(db, entity_id, weight, 1_700_000_000_000u64, "").await;
    }

    /// Seed one `NodeAdded` event with an explicit timestamp and `event_id` suffix, so a single
    /// `entity_id` can be re-ingested (the WAL is append-only; a re-ingest emits the node again).
    async fn seed_node_event_at(
        db: &Surreal<Any>,
        entity_id: &str,
        weight: &NodeWeight,
        timestamp: u64,
        event_suffix: &str,
    ) {
        let weight_json = serde_json::to_string(weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("NodeAdded-{entity_id}{event_suffix}"),
                    "timestamp": timestamp,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed node event")
            .check()
            .expect("seed node event returned a statement error");
    }

    fn location(name: &str, postcode: Option<&str>, address_line_1: Option<&str>) -> NodeWeight {
        NodeWeight::Location(LocationData {
            name: name.into(),
            postcode: postcode.map(str::to_owned),
            address_line_1: address_line_1.map(str::to_owned),
            locality: None,
        })
    }

    #[test]
    fn postcode_sector_matches_python_shape() {
        assert_eq!(
            postcode_sector("EC1A 1BB"),
            Some("EC1A 1".to_string()),
            "already-spaced postcode"
        );
        assert_eq!(
            postcode_sector("ec1a1bb"),
            Some("EC1A 1".to_string()),
            "lowercase, no space"
        );
        assert_eq!(postcode_sector(""), None);
        assert_eq!(postcode_sector("not a postcode"), None);
    }

    #[test]
    fn street_key_strips_leading_number_and_lowercases() {
        assert_eq!(
            street_key("10 Downing Street"),
            Some("downing street".to_string())
        );
        assert_eq!(
            street_key("12A High Street"),
            Some("high street".to_string())
        );
        assert_eq!(street_key(""), None);
    }

    #[tokio::test]
    async fn geographic_concentration_zero_locations_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: geographic_concentration_zero_locations_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        let probe = GeographicConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero Location nodes must never produce Clear (the Severe hazard) — got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn geographic_concentration_distinct_streets_is_clear_with_population_in_evidence() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: geographic_concentration_distinct_streets_is_clear_with_population_in_evidence (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // 5 Location nodes, each on a distinct street, distinct postcode sector.
        let fixtures = [
            ("Site A", "EC1A 1BB", "10 Downing Street"),
            ("Site B", "SW1A 1AA", "20 Baker Street"),
            ("Site C", "W1A 1AA", "5 Marylebone Road"),
            ("Site D", "N1 1AA", "8 Upper Street"),
            ("Site E", "SE1 1AA", "1 Borough High Street"),
        ];
        for (i, (name, postcode, addr)) in fixtures.iter().enumerate() {
            seed_node_event(
                &db,
                &format!("GB-LOC:GEO-CLEAR-{i}"),
                &location(name, Some(postcode), Some(addr)),
            )
            .await;
        }

        let probe = GeographicConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "5 distinct streets must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            finding.evidence.iter().any(|e| e.text.contains('5')),
            "evidence must state the literal population figure, got: {:?}",
            finding.evidence
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // The evidence substring check above is this file's own worked example of Pitfall 3:
        // a bare '5' anywhere in the prose satisfies it. Kept for the evidence-text property,
        // but the compliance proof is the struct assertion below -- matching the pattern this
        // file's OTHER Clear test already used.
        assert_eq!(
            coverage.population(),
            5,
            "population must count all 5 seeded distinct-street locations"
        );
        assert_eq!(
            coverage.capable(),
            5,
            "capable must equal population -- every seeded location has a usable address"
        );
    }

    #[tokio::test]
    async fn geographic_concentration_repeat_street_is_weak_never_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: geographic_concentration_repeat_street_is_weak_never_hit (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Two Locations share the "downing street" street_key.
        seed_node_event(
            &db,
            "GB-LOC:GEO-WEAK-1",
            &location("Site A", Some("EC1A 1BB"), Some("10 Downing Street")),
        )
        .await;
        seed_node_event(
            &db,
            "GB-LOC:GEO-WEAK-2",
            &location("Site B", Some("EC1A 1BB"), Some("12 Downing Street")),
        )
        .await;

        let probe = GeographicConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "a repeated street must be Weak, never Hit — got {:?}",
            finding.verdict
        );
        assert!(
            finding.evidence.iter().any(|e| e.text.contains('1')),
            "evidence must state the repeat-street count, got: {:?}",
            finding.evidence
        );
    }

    /// ADJ-01's regression guard (see this file's header).
    ///
    /// One Location, re-ingested three times, on a street no other Location touches. Under the
    /// old `scan_entities_by_type` this produced three rows, so `streets["downing street"] == 3`
    /// cleared the `>= 2` repeat threshold and the probe reported a coordinated-activity cluster
    /// that does not exist. Under `scan_latest_entities_by_type` it is one location on one
    /// street: `Clear`.
    ///
    /// Deliberately asserts on the VERDICT and the population figure, not on a substring that a
    /// re-worded evidence line could satisfy by accident. The `Weak` case is already covered by
    /// `geographic_concentration_repeat_street_is_weak_never_hit` with two GENUINELY distinct
    /// locations, so this pair distinguishes real repetition from re-ingest volume — the exact
    /// distinction the old scan could not make.
    #[tokio::test]
    async fn geographic_concentration_reingested_location_is_not_a_cluster() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: geographic_concentration_reingested_location_is_not_a_cluster (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // ONE location, emitted three times — the append-only re-ingest shape.
        let loc = location("Site A", Some("EC1A 1BB"), Some("10 Downing Street"));
        for (i, ts) in [1_700_000_000_000u64, 1_700_000_001_000, 1_700_000_002_000]
            .iter()
            .enumerate()
        {
            seed_node_event_at(&db, "GB-LOC:GEO-REINGEST-1", &loc, *ts, &format!("-v{i}")).await;
        }

        let probe = GeographicConcentration;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "a re-ingested location is ONE location on ONE street, not a cluster — counting per \
             event instead of per entity was ADJ-01's 23,322-vs-1,697 defect; got {:?}",
            finding.verdict
        );
        let population = match &finding.verdict {
            Verdict::Clear(coverage) => coverage.population(),
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            population, 1,
            "population must count distinct locations (1), not NodeAdded events (3)"
        );
    }
}
