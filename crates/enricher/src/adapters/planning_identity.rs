//! Planning Identity adapter (IDN-05).
//!
//! Exact-match join: links `director:{slug}` planning-applicant Entity nodes to
//! existing Companies-House-sourced Entity/Officer nodes when their normalised
//! names are identical. On every match, writes BOTH an `identity_links` audit
//! row AND a real `Directs` graph edge — the only pattern in this codebase
//! (`council_registers.rs`) proven visible to the CSR-snapshot motif detectors,
//! which read only `graph_events` `EdgeAdded` rows and never `identity_links`.
//! An `identity_links` row without a graph edge is invisible to detection —
//! exactly the gap this adapter exists to close.
//!
//! ## Exact-match-only policy
//! No fuzzy/phonetic matching anywhere in this module.
//! `normalise_name(a) == normalise_name(b)` is the only comparison performed.
//! Ambiguous normalised-name collisions among CH-sourced candidates resolve to
//! NO match for that name (mirrors `crates/ingestor/src/backfill/ch.rs`'s
//! `MatchVerdict::Ambiguous`) — never an arbitrary pick.
//!
//! ## `director:unknown-applicant`
//! The placeholder mega-node for unattributed planning applications is never a
//! matching candidate — explicitly excluded before the lookup (see
//! [`is_planning_candidate`]).
//!
//! ## Security
//! - T-24-06 (Tampering): `write_identity_link` is reused verbatim — bound
//!   parameters only, `evidence_json` is always a serialised string.
//! - T-24-07 (Repudiation): `write_identity_link(...).await?` is never
//!   discarded (no `let _ =`) — the `identity_links` audit row is the
//!   record-of-truth this adapter protects, mirroring the project's
//!   single most-repeated lesson (the ICIJ adapter wrote 0 rows for its
//!   entire life while a discarded `Result` logged success). `mutation_tx.send`
//!   uses the same `let _ =` pattern as every sibling adapter
//!   (`council_registers.rs`, `land_registry_ccod.rs`, `land_registry_ocod.rs`,
//!   `london_gazette.rs`, `electoral_commission.rs`) because the real safety
//!   net is downstream: `wal_writer.rs::drain_to_wal` fails the whole
//!   enrichment run (bare `?`) on the first real `graph_events` write failure
//!   — a dropped channel send only happens once the channel is already
//!   closed (a shutdown condition), never silent per-row data loss.
//! - T-24-08 (Spoofing / false-positive identity): ambiguous normalised-name
//!   collisions match nothing, never a guess.

use async_trait::async_trait;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info};

use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::stream::normalise_name;
use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;

use crate::adapters::ch_psc::write_identity_link;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{IdentityLinkRecord, LinkStatus, AUTO_LINK_THRESHOLD};

/// The placeholder mega-node for planning applications with no attributed applicant.
/// Never a matching candidate — matching it against anything would be meaningless.
const UNKNOWN_APPLICANT_ID: &str = "director:unknown-applicant";

/// Confidence asserted for an exact match on an *abbreviated* applicant name
/// (one containing an initial, e.g. `"G Jones"`).
///
/// Sits deliberately in the `CANDIDATE_THRESHOLD..AUTO_LINK_THRESHOLD`
/// (0.70..0.85) band, so such links are written to `identity_links` for analyst
/// review but are NOT auto-linked and produce NO `Directs` graph edge.
const ABBREVIATED_NAME_CONFIDENCE: f64 = 0.80;

/// Planning Identity adapter (IDN-05).
///
/// A purely internal graph join — no external credentials required, always
/// enabled (mirrors `LondonGazetteAdapter`'s "no config needed" simplicity,
/// but with zero fields at all).
pub struct PlanningIdentityAdapter;

impl PlanningIdentityAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PlanningIdentityAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EnricherAdapter for PlanningIdentityAdapter {
    fn name(&self) -> &'static str {
        "planning_identity"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        // Single scan of graph_events for every Entity/Officer NodeAdded row,
        // partitioned Rust-side (SurrealDB 3.x RecordId workaround, mirrors
        // ch.rs / opensanctions.rs) into:
        //   (a) CH-sourced candidates (entity_id NOT "director:"-prefixed) —
        //       used to build the exact-match name lookup;
        //   (b) planning director candidates (entity_id "director:"-prefixed,
        //       excluding the unknown-applicant placeholder) — matched
        //       against that lookup.
        // NOTE: `kind = 'NodeAdded'` is deliberately NOT in the WHERE clause.
        // `kind` is indexed (idx_graph_events_kind); combining an indexed equality
        // with a CONTAINS scan in one SELECT triggers a SurrealDB 3.x planner bug
        // that silently returns a truncated — and run-to-run non-deterministic —
        // row set while `count()` on the same predicate stays correct. Measured
        // live against an_anglesey_aebaa785: the indexed form returned 4,505 then
        // 15,954 rows on consecutive runs; the form below returns a stable 30,972.
        // That truncation starved the name lookup and made the join report
        // auto_linked = 0. `kind` is selected and filtered Rust-side instead.
        // Same class of bug as the `entity --ch-number` lookup failure.
        //
        // CR-03: this scan is paginated, not a single fixed `LIMIT 200000`.
        //
        // The old shape had two independent failure modes, and the second defeated the
        // guard meant to catch the first:
        //   1. A fixed limit on a growing table has already silently truncated this project
        //      once — a `LIMIT 100000` dropped nodes as soon as distinct nodes passed
        //      117,060, and the loss was invisible.
        //   2. The `rows.len() == 200000` warning could not fire under the known truncation
        //      mode: SurrealDB truncates the HTTP response *well below* the requested count
        //      when `weight_json` is large (measured at ~11k of ~44k Contract rows), so the
        //      count never equals the limit. The lookup would simply be starved and the
        //      adapter would report a clean `auto_linked = 0` — the exact symptom the
        //      comment above says this module was written to fix.
        //
        // Advance by rows actually returned and stop only on a short page, matching the
        // idiom used by every other scan in this phase.
        const PAGE: usize = 20_000;
        let mut rows: Vec<serde_json::Value> = Vec::new();
        let mut start = 0usize;
        loop {
            let sql = format!(
                // `id` must be in the projection for SurrealDB 3.x to accept `ORDER BY id`.
                "SELECT id, kind AS kind, payload.weight_json AS w, payload.entity_id AS eid \
                 FROM graph_events \
                 WHERE payload.weight_json CONTAINS '\"type\":\"Entity\"' \
                    OR payload.weight_json CONTAINS '\"type\":\"Officer\"' \
                 ORDER BY id LIMIT {PAGE} START {start}"
            );
            let page: Vec<serde_json::Value> = db.query(sql).await?.take(0)?;
            let returned = page.len();
            rows.extend(page);
            if returned < PAGE {
                break;
            }
            start += returned;
        }
        debug!(
            adapter = "planning_identity",
            rows = rows.len(),
            "scanned graph_events for Entity/Officer candidates"
        );

        let mut ch_candidates: Vec<(String, String)> = Vec::new();
        let mut director_candidates: Vec<(String, String)> = Vec::new();

        for row in &rows {
            if row["kind"].as_str() != Some("NodeAdded") {
                continue;
            }
            let eid = row["eid"].as_str().unwrap_or("").to_string();
            if eid.is_empty() {
                continue;
            }
            let wj = row["w"].as_str().unwrap_or("");
            let Ok(d) = serde_json::from_str::<serde_json::Value>(wj) else {
                continue;
            };
            let Some(name) = d.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }

            if is_planning_candidate(&eid) {
                director_candidates.push((eid, name.to_string()));
            } else if !eid.starts_with("director:") {
                ch_candidates.push((eid, name.to_string()));
            }
            // else: eid == UNKNOWN_APPLICANT_ID — excluded from both groups.
        }

        let name_lookup = build_name_lookup(&ch_candidates);
        let outcome = match_planning_identities(&director_candidates, &name_lookup);

        let mut stats = EnrichmentStats {
            candidates_found: outcome.candidates_found,
            ..EnrichmentStats::default()
        };

        for (director_entity_id, applicant_name, matched_id) in &outcome.matched {
            let evidence_json = serde_json::json!({
                "applicant_name": applicant_name,
                "matched_entity": matched_id,
            })
            .to_string();

            // An exact match on an abbreviated name ("G Jones") is a real string
            // equality but not a certain identity claim — see `confidence_for`.
            let confidence = confidence_for(applicant_name);
            let auto_linkable = confidence >= AUTO_LINK_THRESHOLD;

            let link_record = IdentityLinkRecord {
                id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                source_node_id: director_entity_id.clone(),
                target_node_id: matched_id.clone(),
                resolution_type: "planning_applicant_exact_name".to_string(),
                confidence,
                source: "planning_identity".to_string(),
                resolved_at: Utc::now(),
                evidence_json,
                status: if auto_linkable {
                    LinkStatus::AutoLinked
                } else {
                    LinkStatus::PendingReview
                },
            };

            // T-24-07: never `let _ =` here — the identity_links audit row is
            // the record-of-truth this adapter's repudiation mitigation protects.
            // Written for BOTH auto-linked and pending-review links: a link held
            // back from the graph must still be auditable and analyst-actionable
            // via `links confirm/reject`.
            write_identity_link(db, &link_record).await?;

            if !auto_linkable {
                // Sub-threshold links never reach the hot graph mutation path
                // (T-16-05 mitigation, enforced by `ConfirmedLink::try_new` for
                // callers that route through the typed API). Emitting a Directs
                // edge here would make an uncertain identity detector-visible,
                // which is precisely what the threshold ladder exists to prevent.
                stats.pending_review += 1;
                continue;
            }

            // The actual safety net for a dropped mutation_tx.send is downstream
            // in wal_writer.rs::drain_to_wal, which fails the whole enrichment
            // run (bare `?`) on the first real graph_events write failure — a
            // dropped send only occurs once the channel is already closed
            // (shutdown), not the silent per-row data-loss pattern the ICIJ
            // lesson is about. Matches every sibling adapter's discard pattern.
            let _ = mutation_tx
                .send(GraphMutation::AddEdge {
                    src: EntityId(director_entity_id.clone()),
                    dst: EntityId(matched_id.clone()),
                    weight: EdgeWeight::Directs {
                        meta: EdgeMeta {
                            created_at: Utc::now(),
                            effective_from: None,
                            effective_to: None,
                        },
                        data: DirectsData {
                            details: "planning_identity:planning_applicant_exact_name".to_string(),
                            officer_role: None,
                        },
                    },
                })
                .await;

            stats.auto_linked += 1;
        }
        stats.discarded = outcome.discarded;

        info!(
            adapter = "planning_identity",
            candidates = stats.candidates_found,
            auto_linked = stats.auto_linked,
            pending_review = stats.pending_review,
            discarded = stats.discarded,
            "Planning identity enrichment complete"
        );

        Ok(stats)
    }
}

// ── Pure matching logic (unit-testable without a live SurrealDB) ────────────
// Mirrors ch.rs's separation of match_company (pure) from run() (I/O).

/// True if `entity_id` is a planning-applicant director candidate — i.e.
/// `"director:"`-prefixed but NOT the unknown-applicant placeholder, which is
/// never a matching candidate (matching the placeholder mega-node against
/// anything would be meaningless).
pub fn is_planning_candidate(entity_id: &str) -> bool {
    entity_id.starts_with("director:") && entity_id != UNKNOWN_APPLICANT_ID
}

/// True if `name` contains an initial — a single-letter alphabetic token such as
/// the `A` in `"A Rahman"` or the `J`/`E` in `"J E Owen"`.
///
/// An initial means the name was **abbreviated**: identifying information that
/// existed at source was discarded before it reached us. `"A Rahman"` is an
/// exact normalised match for `A.RAHMAN LIMITED`, but it is an equally exact
/// match for any other A. Rahman — the string equality is real, the identity
/// claim behind it is not certain.
///
/// This is deliberately NOT a fuzzy comparison and does not weaken the
/// exact-match-only policy: matching remains strict string equality on
/// normalised names. It only governs how much certainty the resulting link is
/// allowed to assert.
pub fn contains_initial(name: &str) -> bool {
    name.split(|c: char| !c.is_alphanumeric())
        .any(|tok| tok.chars().count() == 1 && tok.chars().all(|c| c.is_alphabetic()))
}

/// Confidence for an exact normalised-name match on `applicant_name`.
///
/// A full, unabbreviated name match is asserted at `1.0`. An abbreviated name
/// (see [`contains_initial`]) is asserted at [`ABBREVIATED_NAME_CONFIDENCE`],
/// which sits below `AUTO_LINK_THRESHOLD` and so routes the link to analyst
/// review instead of auto-linking it into the hot graph.
pub fn confidence_for(applicant_name: &str) -> f64 {
    if contains_initial(applicant_name) {
        ABBREVIATED_NAME_CONFIDENCE
    } else {
        1.0
    }
}

/// Outcome of matching planning-applicant director candidates against the
/// CH-sourced name lookup.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchOutcome {
    /// (director_entity_id, applicant_name, matched_ch_entity_id)
    pub matched: Vec<(String, String, String)>,
    /// Distinct (identity, normalised name) pairs considered — NOT the number of
    /// `graph_events` rows scanned, which repeats an applicant once per event.
    pub candidates_found: usize,
    /// Distinct pairs with no exact CH name match.
    pub discarded: usize,
}

/// Build a `normalise_name(name) -> entity_id` lookup from CH-sourced
/// Entity/Officer candidates.
///
/// On a normalised-name collision between two DIFFERENT entity ids, the key is
/// removed entirely (kept out of the map) rather than picking one arbitrarily
/// — an ambiguous match must never silently resolve to a guess (mirrors
/// `ch.rs`'s `MatchVerdict::Ambiguous`).
pub fn build_name_lookup(ch_candidates: &[(String, String)]) -> HashMap<String, String> {
    let mut lookup: HashMap<String, String> = HashMap::new();
    let mut ambiguous: HashSet<String> = HashSet::new();

    for (entity_id, name) in ch_candidates {
        let norm = normalise_name(name);
        if norm.is_empty() || ambiguous.contains(&norm) {
            continue;
        }
        match lookup.get(&norm) {
            Some(existing) if existing != entity_id => {
                // Two distinct CH entities/officers share a normalised name —
                // ambiguous. Remove the key and never re-add it.
                lookup.remove(&norm);
                ambiguous.insert(norm);
            }
            Some(_) => {} // Same entity_id seen again (duplicate row) — no-op.
            None => {
                lookup.insert(norm, entity_id.clone());
            }
        }
    }

    lookup
}

/// Match planning-applicant director candidates against the CH-sourced name
/// lookup using exact normalised-name equality only — no fuzzy/phonetic path
/// exists anywhere in this function.
pub fn match_planning_identities(
    director_candidates: &[(String, String)],
    name_lookup: &HashMap<String, String>,
) -> MatchOutcome {
    let mut matched = Vec::new();
    let mut discarded = 0usize;
    // `graph_events` carries more than one `NodeAdded` per applicant, so the same identity
    // arrives here repeatedly (3,969 excess events over 42,984 distinct `director:` ids on
    // `an_anglesey_aebaa785`; one id appears 50 times). Every repeat produced another
    // `matched` entry and another `stats.auto_linked` increment, while the write is an
    // UPSERT keyed `(source_node_id, target_node_id, source)` and collapses them onto one
    // row — the adapter's counters were counting write ATTEMPTS, not links, which is the
    // 169-logged-vs-123-persisted gap in Open Item 27.
    //
    // The key is the (identity, normalised name) PAIR, deliberately not the identity alone:
    // that pair is the entire input to the decision below, so deduplicating on it cannot
    // change any match outcome, only remove exact repeats of one. Collapsing on the id
    // would instead drop the second link for an identity carrying two different names,
    // which the UPSERT would legitimately have kept as a separate row.
    let mut seen: HashSet<(&str, String)> = HashSet::new();
    let mut candidates_found = 0usize;

    for (director_entity_id, applicant_name) in director_candidates {
        let norm = normalise_name(applicant_name);
        if !seen.insert((director_entity_id.as_str(), norm.clone())) {
            continue;
        }
        candidates_found += 1;
        match name_lookup.get(&norm) {
            Some(matched_id) => {
                matched.push((
                    director_entity_id.clone(),
                    applicant_name.clone(),
                    matched_id.clone(),
                ));
            }
            None => discarded += 1,
        }
    }

    MatchOutcome {
        candidates_found,
        discarded,
        matched,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── name() / is_enabled() ────────────────────────────────────────────────

    #[test]
    fn test_name_is_planning_identity() {
        let adapter = PlanningIdentityAdapter::new();
        assert_eq!(adapter.name(), "planning_identity");
    }

    #[test]
    fn test_always_enabled() {
        let adapter = PlanningIdentityAdapter::new();
        assert!(
            adapter.is_enabled(),
            "planning_identity has no config/credentials — always enabled"
        );
    }

    // ── is_planning_candidate ─────────────────────────────────────────────────

    #[test]
    fn test_unknown_applicant_never_a_candidate() {
        assert!(
            !is_planning_candidate(UNKNOWN_APPLICANT_ID),
            "director:unknown-applicant must never be a matching candidate"
        );
    }

    #[test]
    fn test_director_entity_is_a_candidate() {
        assert!(is_planning_candidate("director:acme-developments"));
    }

    #[test]
    fn test_ch_entity_is_not_a_planning_candidate() {
        assert!(!is_planning_candidate("GB-COH:12345678"));
        assert!(!is_planning_candidate("OFF:12345"));
    }

    // ── build_name_lookup / match_planning_identities: exact match ─────────────

    #[test]
    fn test_exact_match_produces_link() {
        let ch_candidates = vec![(
            "GB-COH:12345678".to_string(),
            "ACME DEVELOPMENTS LTD".to_string(),
        )];
        let name_lookup = build_name_lookup(&ch_candidates);

        let directors = vec![(
            "director:acme-developments".to_string(),
            "Acme Developments".to_string(),
        )];
        let outcome = match_planning_identities(&directors, &name_lookup);

        assert_eq!(outcome.candidates_found, 1);
        assert_eq!(outcome.discarded, 0);
        assert_eq!(outcome.matched.len(), 1);
        assert_eq!(outcome.matched[0].0, "director:acme-developments");
        assert_eq!(outcome.matched[0].1, "Acme Developments");
        assert_eq!(outcome.matched[0].2, "GB-COH:12345678");
    }

    // ── no match → discarded ────────────────────────────────────────────────

    #[test]
    fn test_no_match_increments_discarded() {
        let name_lookup = HashMap::new();
        let directors = vec![(
            "director:nobody-known".to_string(),
            "Nobody Known Ltd".to_string(),
        )];
        let outcome = match_planning_identities(&directors, &name_lookup);

        assert_eq!(outcome.candidates_found, 1);
        assert_eq!(outcome.discarded, 1);
        assert!(outcome.matched.is_empty());
    }

    // ── ambiguous normalised-name collision → neither candidate matched ────────

    #[test]
    fn test_ambiguous_collision_matches_neither() {
        let ch_candidates = vec![
            ("GB-COH:11111111".to_string(), "Acme Ltd".to_string()),
            ("GB-COH:22222222".to_string(), "ACME LIMITED".to_string()),
        ];
        let name_lookup = build_name_lookup(&ch_candidates);

        assert!(
            !name_lookup.contains_key(&normalise_name("Acme Ltd")),
            "ambiguous normalised-name collision must be removed from the lookup entirely"
        );

        let directors = vec![("director:acme".to_string(), "Acme Ltd".to_string())];
        let outcome = match_planning_identities(&directors, &name_lookup);

        assert!(
            outcome.matched.is_empty(),
            "an ambiguous name must resolve to NO match, never an arbitrary pick"
        );
        assert_eq!(outcome.discarded, 1);
    }

    #[test]
    fn test_build_name_lookup_no_collision_when_same_entity_repeated() {
        // The same CH entity appearing twice (e.g. duplicate NodeAdded rows)
        // is not an ambiguous collision — it is the same target both times.
        let ch_candidates = vec![
            ("GB-COH:11111111".to_string(), "Acme Ltd".to_string()),
            ("GB-COH:11111111".to_string(), "Acme Ltd".to_string()),
        ];
        let name_lookup = build_name_lookup(&ch_candidates);
        assert_eq!(
            name_lookup.get(&normalise_name("Acme Ltd")),
            Some(&"GB-COH:11111111".to_string())
        );
    }

    #[test]
    fn test_build_name_lookup_skips_empty_normalised_name() {
        let ch_candidates = vec![("GB-COH:11111111".to_string(), "Ltd".to_string())];
        let name_lookup = build_name_lookup(&ch_candidates);
        assert!(
            name_lookup.is_empty(),
            "a name that normalises to empty must not populate the lookup"
        );
    }

    // ── No fuzzy/phonetic path — structural check ───────────────────────────

    #[test]
    fn test_near_miss_name_does_not_match() {
        // "Acme Developments" vs "Acme Development" (singular) — a fuzzy/phonetic
        // matcher might link these; exact normalised-name equality must not.
        let ch_candidates = vec![(
            "GB-COH:99999999".to_string(),
            "Acme Development Ltd".to_string(),
        )];
        let name_lookup = build_name_lookup(&ch_candidates);
        let directors = vec![(
            "director:acme-developments".to_string(),
            "Acme Developments".to_string(),
        )];
        let outcome = match_planning_identities(&directors, &name_lookup);
        assert!(
            outcome.matched.is_empty(),
            "near-miss names must not match under exact-only equality"
        );
        assert_eq!(outcome.discarded, 1);
    }

    // ── Confidence: abbreviated names do not assert certainty ──────────────

    #[test]
    fn test_contains_initial_detects_abbreviated_names() {
        for name in ["A Rahman", "G Jones", "J E Owen", "A.RAHMAN", "J. D. Smith"] {
            assert!(
                contains_initial(name),
                "{name:?} contains an initial and must be detected as abbreviated"
            );
        }
    }

    #[test]
    fn test_contains_initial_ignores_full_names() {
        // Every one of these is a real applicant name that matched live against
        // an_anglesey_aebaa785 — they must keep full confidence.
        for name in [
            "Ford Motor Company",
            "National Westminster Bank",
            "National Westminster Bank Plc.",
            "ROYAL BRITISH LEGION",
            "North Wales Wildlife Trust",
            "HEALTH & SAFETY EXECUTIVE",
            "Openreach",
            "BT PLC",
        ] {
            assert!(
                !contains_initial(name),
                "{name:?} is unabbreviated and must retain confidence 1.0"
            );
        }
    }

    #[test]
    fn test_confidence_for_abbreviated_name_is_below_auto_link_threshold() {
        // The whole point: an abbreviated match must NOT auto-link, so it never
        // reaches the hot graph as an unreviewed identity claim.
        for name in ["A Rahman", "G Jones", "J E Owen"] {
            let c = confidence_for(name);
            assert!(
                c < AUTO_LINK_THRESHOLD,
                "{name:?} scored {c}, which would auto-link"
            );
            assert!(
                c >= crate::confidence::CANDIDATE_THRESHOLD,
                "{name:?} scored {c}, which would drop below analyst review entirely"
            );
        }
    }

    #[test]
    fn test_confidence_for_full_name_auto_links() {
        for name in ["Ford Motor Company", "Openreach", "ROYAL BRITISH LEGION"] {
            assert_eq!(confidence_for(name), 1.0);
            assert!(confidence_for(name) >= AUTO_LINK_THRESHOLD);
        }
    }

    #[test]
    fn test_confidence_change_does_not_weaken_exact_matching() {
        // Guard against a future refactor turning the confidence tier into a
        // matching relaxation: "A Rahman" vs "B Rahman" must still not match at
        // any confidence, because matching remains strict string equality.
        let ch_candidates = vec![(
            "GB-COH:16367366".to_string(),
            "B.RAHMAN LIMITED".to_string(),
        )];
        let name_lookup = build_name_lookup(&ch_candidates);
        let directors = vec![("director:a-rahman".to_string(), "A Rahman".to_string())];
        let outcome = match_planning_identities(&directors, &name_lookup);
        assert!(
            outcome.matched.is_empty(),
            "differing initials must never match — confidence tiering is not fuzzy matching"
        );
    }

    // ── repeated events for one applicant (Open Item 27, counter half) ────────

    /// `graph_events` holds more than one `NodeAdded` per applicant — 3,969 excess events
    /// over 42,984 distinct `director:` ids on `an_anglesey_aebaa785`, one id appearing 50
    /// times. The scan feeds every occurrence in, so the same identity was matched once per
    /// event while `write_identity_link`'s UPSERT collapsed them all onto the one row keyed
    /// `(source_node_id, target_node_id, source)`. That is the 169-logged-vs-123-persisted
    /// discrepancy: the counters were counting write attempts, not links.
    #[test]
    fn repeated_events_for_one_applicant_count_once() {
        let ch_candidates = vec![("GB-COH:01234567".to_string(), "Acme Ltd".to_string())];
        let name_lookup = build_name_lookup(&ch_candidates);
        let directors = vec![
            ("director:acme".to_string(), "Acme Ltd".to_string()),
            ("director:acme".to_string(), "Acme Ltd".to_string()),
            // Same normalised key, different spelling — still the same match attempt.
            ("director:acme".to_string(), "ACME LIMITED".to_string()),
        ];
        let outcome = match_planning_identities(&directors, &name_lookup);
        assert_eq!(
            outcome.candidates_found, 1,
            "three events for one applicant identity are one candidate"
        );
        assert_eq!(
            outcome.matched.len(),
            1,
            "and one link, which is what the UPSERT actually persists"
        );
    }

    /// Dedup is on the (identity, normalised name) PAIR, not the identity alone: one id
    /// carrying two genuinely different names can legitimately match two companies, and
    /// collapsing on the id would silently drop the second link the UPSERT would have kept.
    #[test]
    fn one_applicant_with_two_distinct_names_keeps_both_matches() {
        let ch_candidates = vec![
            ("GB-COH:01234567".to_string(), "Acme Ltd".to_string()),
            ("GB-COH:07654321".to_string(), "Beta Ltd".to_string()),
        ];
        let name_lookup = build_name_lookup(&ch_candidates);
        let directors = vec![
            ("director:acme".to_string(), "Acme Ltd".to_string()),
            ("director:acme".to_string(), "Beta Ltd".to_string()),
        ];
        let outcome = match_planning_identities(&directors, &name_lookup);
        assert_eq!(outcome.candidates_found, 2);
        assert_eq!(outcome.matched.len(), 2);
    }

    /// The unmatched count is a count of identities too, not of events.
    #[test]
    fn repeated_events_for_an_unmatched_applicant_discard_once() {
        let name_lookup = build_name_lookup(&[]);
        let directors = vec![
            ("director:nowhere".to_string(), "Nowhere Ltd".to_string()),
            ("director:nowhere".to_string(), "Nowhere Ltd".to_string()),
        ];
        let outcome = match_planning_identities(&directors, &name_lookup);
        assert_eq!(outcome.candidates_found, 1);
        assert_eq!(outcome.discarded, 1);
    }
}
