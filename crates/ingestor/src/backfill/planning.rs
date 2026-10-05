//! `backfill planning` — repair planning Contracts whose applicant attribution is either
//! unresolved (`AwardedTo` still points at the `director:unknown-applicant` placeholder) or
//! simply predates the `applicant_provenance` field (IDN-04, Phase 24 Plan 05).
//!
//! Two independent passes, deliberately kept apart:
//!
//! 1. **Edge-scoped re-scrape pass** (steps 1-6). The re-scrape-eligible set is determined by
//!    each Contract's CURRENT `AwardedTo` edge target — NOT by `applicant_provenance.is_none()`.
//!    Only Contracts whose `AwardedTo` edge still points at `director:unknown-applicant` are
//!    eligible (~2,005 live). A naive provenance-absent gate would match the full ~39,860-row
//!    corpus and, because `graph_events` is append-only, would insert ~75,000 duplicate
//!    `AwardedTo`/`LocatedAt` edges for records that were already correctly attributed.
//!    Eligible Contracts are re-listed ward-by-ward (only wards that actually have at least one
//!    placeholder-attributed Contract), their provenance is mutated in place, and newly-resolved
//!    applicants get a NEW `AwardedTo` + `LocatedAt` edge pair. The old placeholder edges are
//!    never mutated and never deleted (edges are temporal).
//!
//! 2. **Bulk-stamp pass** (step 7). Planning Contracts NOT in the edge-scoped set already have an
//!    `AwardedTo` edge to a real `director:{slug}` from the original ingest, so their applicant
//!    was published. Those whose `applicant_provenance` is still absent are stamped `Published`
//!    via `update_node_event` alone — no sidecar call, no new entity, no new edge, zero new
//!    `graph_events` rows.
//!
//! Counters: `matched` = provenance corrections driven by the re-scrape (edge insertions are a
//! SUBSET of `matched` — only those where the applicant became newly resolvable);
//! `skipped` = provenance corrected to `FetchFailed` for records the re-scrape no longer returns
//! at all (never a silent no-op); `already_resolved` = re-scrape-eligible Contracts skipped by the
//! step-(5a) edge-existence guard because a previous pass already resolved them;
//! `bulk_stamped` = mutation-only `Published` stamps from pass 2. Contracts counted in
//! `bulk_stamped` NEVER receive a new edge insertion.

use anyhow::Result;
use std::collections::{HashMap, HashSet};

use urban_integrity_core::config::PlanningPortalConfig;
use urban_integrity_schema::types::{
    edge::{AwardedToData, EdgeMeta, EdgeWeight, LocatedAtData},
    node::{ApplicantProvenance, ContractData, EntityData, EntityStatus, LocationData, NodeWeight},
};

use crate::backfill::{
    extract_id_str, insert_edge_event, insert_node_event, update_node_event_by_record,
    BackfillStats, DbSession,
};
use crate::planning::mapper::{
    applicant_slug, derive_applicant_provenance, extract_postcode, location_id_for,
};
use crate::planning::sidecar::{PlanningPortalItem, ScrapeFailure, ScrapeOutcome, ScrapingSidecar};

/// The placeholder applicant entity created by `map_planning_item` when a portal publishes no
/// applicant name. Presence of an `AwardedTo` edge FROM this id is the sole gate determining
/// re-scrape eligibility.
pub const UNKNOWN_APPLICANT_ID: &str = "director:unknown-applicant";

/// Page size for the paginated `graph_events` scans. Both scans advance by rows actually
/// returned, never assuming a single page.
const SCAN_PAGE_SIZE: usize = 20_000;

/// Page size for the per-entity existence checks (CR-06). Small because these read all events
/// for a single `entity_id` and stop at the first `NodeAdded`; the loop, not the size, is what
/// makes the check sound.
const EXISTENCE_PAGE_SIZE: usize = 200;

// ── data carried between the scan and the mutation passes ─────────────────────

/// One re-scrape-eligible Contract, resolved from the edge-scoped scan (step 1) plus its LATEST
/// `NodeAdded` row (step 2).
#[derive(Debug, Clone, PartialEq)]
pub struct UnresolvedContract {
    /// SurrealDB **record id** (`graph_events:…`) of the LATEST `NodeAdded` row for this Contract
    /// — the row to mutate. A record id, not an `event_id`: mutation goes through
    /// `update_node_event_by_record`, which addresses the row by primary key.
    pub record_id: String,
    /// `planning:{reference}`
    pub entity_id: String,
    /// Portal application reference used to match freshly-scraped items.
    pub application_ref: String,
    /// ONS GSS ward code, used to pick the portal to re-list.
    pub ward_code: String,
    pub data: ContractData,
}

/// A planning Contract eligible for the mutation-only bulk stamp (step 7).
#[derive(Debug, Clone, PartialEq)]
pub struct BulkStampCandidate {
    /// SurrealDB **record id** (`graph_events:…`) of the LATEST `NodeAdded` row for this Contract.
    pub record_id: String,
    pub entity_id: String,
    pub data: ContractData,
}

// ── DB operations, behind a trait so the decision logic is unit-testable ──────

/// The five `graph_events` operations this backfiller performs. Behind a trait so the
/// mutation/insertion decision logic can be unit-tested with no live DB — in particular so the
/// bulk-stamp and edge-existence-guard branches can assert that `insert_edge` is never called.
///
/// T-24-13: every method returns `Result` and every call site propagates it. A `let _ = ...` on a
/// SurrealDB write is forbidden in this crate (see the ICIJ adapter incident: 10h of silent no-op
/// writes while the log claimed 814k rows written).
#[async_trait::async_trait]
pub trait BackfillOps: Send + Sync {
    /// Does a `NodeAdded` row already exist for this entity id?
    async fn entity_exists(&self, entity_id: &str) -> Result<bool>;
    /// Does an `EdgeAdded` row already exist for this exact (src, dst) pair?
    async fn edge_exists(&self, src_id: &str, dst_id: &str) -> Result<bool>;
    async fn update_node(&self, event_id: &str, weight: &NodeWeight) -> Result<()>;
    async fn insert_node(&self, entity_id: &str, weight: &NodeWeight) -> Result<()>;
    async fn insert_edge(&self, src_id: &str, dst_id: &str, weight: &EdgeWeight) -> Result<()>;
}

/// Live SurrealDB implementation. Reuses `backfill::mod`'s `update_node_event` /
/// `insert_node_event` / `insert_edge_event` verbatim — none of them are reimplemented here.
/// When `dry_run` is set the three write methods are no-ops; the two existence checks still run
/// (they are read-only) so a dry run reports the same `already_resolved` classification a live
/// run would produce against the current DB state.
pub struct SurrealOps {
    session: DbSession,
    dry_run: bool,
}

#[async_trait::async_trait]
impl BackfillOps for SurrealOps {
    async fn entity_exists(&self, entity_id: &str) -> Result<bool> {
        // T-24-12: the slug is derived from a portal-sourced applicant name — bound parameter,
        // never interpolated into the SurrealQL.
        //
        // `kind` is deliberately NOT in the WHERE clause. `kind` is indexed
        // (idx_graph_events_kind), and pairing that indexed equality with a further equality on an
        // UNINDEXED field makes SurrealDB 3.x take the index path and return an empty row set,
        // while `count()` on the identical predicate correctly returns 1. Measured live against
        // an_anglesey_aebaa785 for one applicant director node: the indexed form returned [] in 3ms;
        // the form below returned the row in 1.06s; `count()` said 1.
        //
        // This is the same planner defect as the wave-1 scan failure, but it is NOT confined to
        // CONTAINS predicates as that fix assumed — plain equality triggers it too. A false `false`
        // here means a duplicate entity node is inserted into an append-only log on every re-run.
        // `kind` is selected and filtered Rust-side instead.
        //
        // CR-06: this previously used a bare `LIMIT 8` with no ORDER BY, which made the check
        // unsound in exactly the case that matters. `NodeUpdated` rows carry `payload.entity_id`
        // too (see `core::persistence::surreal`), and enrichment writes many per entity — status
        // transitions, Gazette InLiquidation, CH merges. An entity with 8+ such events could
        // return an arbitrary, all-`NodeUpdated` subset and answer `false` despite having a
        // `NodeAdded` row. The caller then inserts a BARE `EntityData` (no company number,
        // status Active) stamped `now()`, which wins latest-wins replay and resurrects a
        // Dissolved company — silently disabling the Phoenix detector for it.
        //
        // Paginate instead, filtering `kind` Rust-side, and stop at the first `NodeAdded`.
        // Realistically this exits on the first page; correctness no longer depends on that.
        let mut start = 0usize;
        loop {
            let sql = format!(
                // `id` must appear in the projection — SurrealDB 3.x rejects an ORDER BY idiom
                // that is not selected. Same idiom as `scan_placeholder_attributed_contracts`.
                "SELECT id, kind AS kind FROM graph_events \
                 WHERE payload.entity_id = $id \
                 ORDER BY id LIMIT {EXISTENCE_PAGE_SIZE} START {start}"
            );
            let rows: Vec<serde_json::Value> = self
                .session
                .query(&sql, &[("id", serde_json::json!(entity_id))])
                .await?
                .take(0)?;
            let returned = rows.len();
            if rows.iter().any(|r| r["kind"].as_str() == Some("NodeAdded")) {
                return Ok(true);
            }
            if returned < EXISTENCE_PAGE_SIZE {
                return Ok(false);
            }
            start += returned;
        }
    }

    async fn edge_exists(&self, src_id: &str, dst_id: &str) -> Result<bool> {
        // Step-(5a) edge-existence guard. Both ids are bound parameters, never interpolated.
        //
        // `kind` is deliberately NOT in the WHERE clause — see the note in `entity_exists`. This
        // guard is the single thing preventing a re-run from appending duplicate AwardedTo +
        // LocatedAt pairs to the append-only log, and with `kind = 'EdgeAdded'` in the WHERE
        // clause it returned `false` for EVERY live row: measured against an_anglesey_aebaa785,
        // one applicant -> planning edge returned [] in 3ms with the indexed form
        // and 1 row in 1.19s without it. The unit tests could not catch this — they exercise the
        // mock BackfillOps, so this SurrealQL string never reaches a real database in test.
        let rows: Vec<serde_json::Value> = self
            .session
            .query(
                "SELECT kind AS kind, event_id FROM graph_events \
                 WHERE payload.src_id = $src_id \
                   AND payload.dst_id = $dst_id \
                 LIMIT 8",
                &[
                    ("src_id", serde_json::json!(src_id)),
                    ("dst_id", serde_json::json!(dst_id)),
                ],
            )
            .await?
            .take(0)?;
        Ok(rows.iter().any(|r| r["kind"].as_str() == Some("EdgeAdded")))
    }

    async fn update_node(&self, record_id: &str, weight: &NodeWeight) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        update_node_event_by_record(&self.session, record_id, weight).await
    }

    async fn insert_node(&self, entity_id: &str, weight: &NodeWeight) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        insert_node_event(&self.session, entity_id, weight).await
    }

    async fn insert_edge(&self, src_id: &str, dst_id: &str, weight: &EdgeWeight) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        insert_edge_event(&self.session, src_id, dst_id, weight).await
    }
}

// ── the backfiller ────────────────────────────────────────────────────────────

pub struct PlanningBackfiller {
    session: DbSession,
    sidecar: ScrapingSidecar,
    portals: Vec<PlanningPortalConfig>,
    pub dry_run: bool,
}

impl PlanningBackfiller {
    pub fn new(
        session: DbSession,
        sidecar: ScrapingSidecar,
        portals: Vec<PlanningPortalConfig>,
        dry_run: bool,
    ) -> Self {
        Self {
            session,
            sidecar,
            portals,
            dry_run,
        }
    }

    pub async fn run(&self) -> Result<BackfillStats> {
        let ops = SurrealOps {
            session: self.session.clone(),
            dry_run: self.dry_run,
        };
        let mut stats = BackfillStats::default();

        // ── 1. Edge-scoped scan: Contracts still attributed to the placeholder ────
        let placeholder_contract_ids = self.scan_placeholder_attributed_contracts().await?;
        stats.scanned = placeholder_contract_ids.len();
        tracing::info!(
            count = stats.scanned,
            "Planning backfill: Contracts still attributed to {UNKNOWN_APPLICANT_ID}"
        );
        if placeholder_contract_ids.is_empty() {
            // Fail loud rather than silently reporting a clean run: an empty scan against a
            // populated Anglesey DB means the scan predicate stopped matching, not that the
            // corpus is clean.
            tracing::warn!(
                "Planning backfill: edge-scoped scan returned zero Contracts — verify the \
                 AwardedTo/{UNKNOWN_APPLICANT_ID} predicate before treating this as a clean run"
            );
        }

        // ── 2. Resolve each to its LATEST NodeAdded row ──────────────────────────
        //
        // ONE scan feeds both passes. Issuing a bounded query per placeholder Contract instead
        // (~2,005 sequential round-trips) took 29 minutes live — `payload.entity_id` is unindexed,
        // so each is a full scan of a 725k-row table — and the run then died on
        // `401 Unauthorized for url (http://localhost:8000/rpc)`: the SurrealDB session token
        // expires under a long-running connection (same failure as the ICIJ drain-session 401).
        // The step-(7) pass already had to scan every Contract row anyway (~3 min), so sharing
        // that one scan removes the 2,005 queries outright rather than papering over the expiry.
        let latest_by_entity = self.scan_contract_latest_rows().await?;
        let mut unresolved: Vec<UnresolvedContract> = Vec::new();
        for entity_id in &placeholder_contract_ids {
            match latest_by_entity.get(entity_id).cloned() {
                Some((record_id, data)) => {
                    let application_ref = application_ref_for(entity_id, &data);
                    let ward_code = match data.ward_code.clone() {
                        Some(w) if !w.is_empty() => w,
                        _ => {
                            tracing::warn!(
                                contract = %entity_id,
                                "no ward_code on Contract — cannot ward-scope a re-list"
                            );
                            stats.errors += 1;
                            continue;
                        }
                    };
                    let Some(application_ref) = application_ref else {
                        tracing::warn!(
                            contract = %entity_id,
                            "no application_ref on Contract — cannot match against a re-list"
                        );
                        stats.errors += 1;
                        continue;
                    };
                    unresolved.push(UnresolvedContract {
                        record_id,
                        entity_id: entity_id.clone(),
                        application_ref,
                        ward_code,
                        data,
                    });
                }
                None => {
                    tracing::warn!(
                        contract = %entity_id,
                        "placeholder AwardedTo edge with no NodeAdded row — skipping"
                    );
                    stats.errors += 1;
                }
            }
        }

        // Every entity_id reached by the edge-scoped pass — the exclusion set for step 7.
        let edge_scoped_ids: HashSet<String> = placeholder_contract_ids.iter().cloned().collect();

        // ── 3. Group by ward; only wards with at least one placeholder-attributed ─
        //       Contract are re-listed (T-24-14: never an unscoped/all-wards query).
        let by_ward = group_by_ward(unresolved);
        tracing::info!(
            wards = by_ward.len(),
            "Planning backfill: wards needing a re-list"
        );

        let mut inserted_entities: HashSet<String> = HashSet::new();
        // CR-04: deduped across the whole run, like `inserted_entities` — many planning
        // records in a ward share one address, and the Location node is written once.
        let mut inserted_locations: HashSet<String> = HashSet::new();

        for (ward_code, ward_map) in &by_ward {
            let Some(portal) = self.portals.iter().find(|p| &p.ward_code == ward_code) else {
                tracing::warn!(
                    ward = %ward_code,
                    contracts = ward_map.len(),
                    "ward not present in config.ingest.planning_portals — cannot re-list; \
                     leaving provenance untouched rather than stamping a value we did not measure"
                );
                stats.errors += ward_map.len();
                continue;
            };

            // ── 4. Ward-scoped re-list via the existing sidecar client ───────────
            tracing::info!(ward = %ward_code, url = %portal.url, "re-listing ward");
            let outcome = self
                .sidecar
                .scrape_checked(&portal.url, &portal.portal_type)
                .await;
            let items = match ward_action(outcome) {
                WardAction::Process(items) => items,
                WardAction::SkipUnmeasured(failure) => {
                    // The re-list FAILED — we learned nothing about this ward. Stamping
                    // fetch_failed here would assert a measurement we never took: it looks
                    // identical to a portal that genuinely dropped the records. Leave
                    // provenance untouched and count the whole ward as errors, exactly as the
                    // missing-config branch above does.
                    tracing::warn!(
                        ward = %ward_code,
                        contracts = ward_map.len(),
                        ?failure,
                        "ward re-list FAILED — leaving provenance untouched rather than \
                         stamping fetch_failed on records we did not measure"
                    );
                    stats.errors += ward_map.len();
                    continue;
                }
            };
            if items.is_empty() {
                tracing::warn!(
                    ward = %ward_code,
                    contracts = ward_map.len(),
                    "ward re-list returned no items — every unresolved Contract in this ward \
                     will be stamped fetch_failed"
                );
                stats.errors += 1;
            }

            // ── 5/5a/6. Apply the mutation/insertion logic for this ward ─────────
            process_ward(
                &ops,
                ward_map,
                &items,
                &mut inserted_entities,
                &mut inserted_locations,
                &mut stats,
                self.dry_run,
            )
            .await?;
        }

        // ── 7. Bulk-stamp pass ───────────────────────────────────────────────────
        // Reuses the step-(2) scan — no second pass over graph_events.
        let candidates = bulk_stamp_candidates(&latest_by_entity, &edge_scoped_ids);
        tracing::info!(
            count = candidates.len(),
            "Planning backfill: Contracts eligible for the mutation-only Published stamp"
        );
        bulk_stamp(&ops, &candidates, &edge_scoped_ids, &mut stats).await?;

        tracing::info!(
            scanned = stats.scanned,
            matched = stats.matched,
            skipped = stats.skipped,
            bulk_stamped = stats.bulk_stamped,
            already_resolved = stats.already_resolved,
            errors = stats.errors,
            dry_run = self.dry_run,
            "Planning backfill complete"
        );
        Ok(stats)
    }

    /// Step (1): paginated scan of `graph_events` for `AwardedTo` edges whose `src_id` is still
    /// the `director:unknown-applicant` placeholder, returning the DISTINCT target Contract ids.
    ///
    /// Deduplication is load-bearing: the append-only log holds duplicate edge rows (live: 2,194
    /// rows for 2,005 distinct `dst_id`s), so `scanned` must be counted off distinct Contracts,
    /// not off raw row count.
    ///
    /// NOTE: `kind = 'EdgeAdded'` is deliberately NOT in the WHERE clause. `kind` is indexed
    /// (`idx_graph_events_kind`); combining an indexed equality with a `CONTAINS` scan in one
    /// SELECT triggers a SurrealDB 3.x planner bug that silently returns a truncated — and
    /// run-to-run non-deterministic — row set while `count()` on the same predicate stays correct
    /// (measured live against `an_anglesey_aebaa785`: 4,505 then 15,954 rows on consecutive runs
    /// where the true count was 30,972). `kind` is selected and filtered Rust-side instead. Same
    /// class of bug as the `entity --ch-number` lookup failure; same workaround as
    /// `crates/enricher/src/adapters/planning_identity.rs`. `payload.src_id` is NOT indexed, so
    /// pairing that equality with the CONTAINS is safe.
    async fn scan_placeholder_attributed_contracts(&self) -> Result<Vec<String>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        let mut start = 0usize;
        loop {
            let sql = format!(
                // `id` must appear in the projection: SurrealDB 3.x rejects an
                // ORDER BY idiom that is not selected ("Missing order idiom `id`
                // in statement selection").
                "SELECT id, kind AS kind, payload.dst_id AS dst FROM graph_events \
                 WHERE payload.weight_json CONTAINS '\"type\":\"AwardedTo\"' \
                   AND payload.src_id = $src_id \
                 ORDER BY id LIMIT {SCAN_PAGE_SIZE} START {start}"
            );
            let rows: Vec<serde_json::Value> = self
                .session
                .query(&sql, &[("src_id", serde_json::json!(UNKNOWN_APPLICANT_ID))])
                .await?
                .take(0)?;
            let returned = rows.len();
            for row in &rows {
                if row["kind"].as_str() != Some("EdgeAdded") {
                    continue;
                }
                let Some(dst) = extract_id_str(&row["dst"]) else {
                    continue;
                };
                if seen.insert(dst.clone()) {
                    out.push(dst);
                }
            }
            if returned < SCAN_PAGE_SIZE {
                break;
            }
            // Advance by rows actually returned — never assume a full page.
            start += returned;
        }
        Ok(out)
    }

    /// The single `graph_events` scan both passes read from: every PLANNING Contract's LATEST
    /// `NodeAdded` row, keyed by `entity_id`, as `(record_id, ContractData)`.
    ///
    /// Latest-wins is load-bearing, not cosmetic. `graph_events` is append-only and this dataset
    /// was ingested more than once: live, 33,287 of 43,719 Contract entities carry more than one
    /// `NodeAdded` row (up to 4). WAL replay and the 24-03 probe
    /// (`scripts/analyst_probe.py::fetch_nodes`, which dedupes by `entity_id` keeping max
    /// `timestamp`) both read latest-wins. Stamping any other row would leave the row the probe
    /// actually reads unstamped, so `not_yet_measured` would stay non-zero after a correct-looking
    /// run.
    ///
    /// Two further corrections against the plan's literal wording, both forced by live
    /// measurement:
    ///   * the raw predicate returns 103,458 rows for 43,719 distinct Contracts — undeduped,
    ///     `bulk_stamped` would report near 100k and write to rows nothing ever reads;
    ///   * restrict to `planning:`-prefixed ids — ~3,859 Contracts Finder award records also carry
    ///     `"type":"Contract"`, and stamping `Published` onto them would assert a provenance that
    ///     was never measured for them (the probe filters on the same prefix).
    ///
    /// `kind` is omitted from the WHERE clause and filtered Rust-side (SurrealDB 3.x planner bug —
    /// see `scan_placeholder_attributed_contracts`).
    async fn scan_contract_latest_rows(&self) -> Result<HashMap<String, (String, ContractData)>> {
        // entity_id -> (timestamp, record_id, weight_json)
        let mut latest: HashMap<String, (i64, String, String)> = HashMap::new();
        let mut start = 0usize;
        loop {
            let sql = format!(
                // `id` must appear in the projection — see the note in
                // `scan_placeholder_attributed_contracts`.
                "SELECT id, kind AS kind, event_id AS event_id, timestamp AS ts, \
                        payload.entity_id AS eid, payload.weight_json AS w \
                 FROM graph_events \
                 WHERE payload.weight_json CONTAINS '\"type\":\"Contract\"' \
                 ORDER BY id LIMIT {SCAN_PAGE_SIZE} START {start}"
            );
            let rows: Vec<serde_json::Value> = self.session.query(&sql, &[]).await?.take(0)?;
            let returned = rows.len();
            for row in &rows {
                if row["kind"].as_str() != Some("NodeAdded") {
                    continue;
                }
                let Some(eid) = extract_id_str(&row["eid"]) else {
                    continue;
                };
                if !eid.starts_with("planning:") {
                    continue;
                }
                let Some(record_id) = extract_id_str(&row["id"]) else {
                    continue;
                };
                let Some(wj) = row["w"].as_str() else {
                    continue;
                };
                let ts = row["ts"].as_i64().unwrap_or(0);
                match latest.get(&eid) {
                    Some((prev_ts, _, _)) if *prev_ts >= ts => {}
                    _ => {
                        latest.insert(eid, (ts, record_id, wj.to_owned()));
                    }
                }
            }
            if returned < SCAN_PAGE_SIZE {
                break;
            }
            start += returned;
        }

        let mut out: HashMap<String, (String, ContractData)> = HashMap::new();
        for (entity_id, (_, record_id, wj)) in latest {
            let Ok(NodeWeight::Contract(data)) = serde_json::from_str::<NodeWeight>(&wj) else {
                continue;
            };
            out.insert(entity_id, (record_id, data));
        }
        Ok(out)
    }
}

/// Step (7) selection, pure: every planning Contract in the shared scan that is NOT in the
/// edge-scoped set and whose `applicant_provenance` is still absent.
///
/// Contracts NOT in the edge-scoped set have, by construction, an `AwardedTo` edge to a real
/// `director:{slug}` from the original ingest — so their applicant was published and no
/// re-scrape, existence check, or edge insertion is needed or performed.
fn bulk_stamp_candidates(
    latest_by_entity: &HashMap<String, (String, ContractData)>,
    edge_scoped_ids: &HashSet<String>,
) -> Vec<BulkStampCandidate> {
    let mut out: Vec<BulkStampCandidate> = Vec::new();
    for (entity_id, (record_id, data)) in latest_by_entity {
        if edge_scoped_ids.contains(entity_id) || data.applicant_provenance.is_some() {
            continue;
        }
        out.push(BulkStampCandidate {
            record_id: record_id.clone(),
            entity_id: entity_id.clone(),
            data: data.clone(),
        });
    }
    out
}

// ── pure helpers (unit-tested with no live DB) ────────────────────────────────

/// The portal application reference used to match a Contract against freshly-scraped items.
/// Prefers the persisted `application_ref`; falls back to the `planning:{reference}` entity id,
/// which is how `map_planning_item` mints the key.
pub(crate) fn application_ref_for(entity_id: &str, data: &ContractData) -> Option<String> {
    match data.application_ref.as_deref() {
        Some(r) if !r.is_empty() => Some(r.to_owned()),
        _ => entity_id
            .strip_prefix("planning:")
            .filter(|r| !r.is_empty())
            .map(str::to_owned),
    }
}

/// Group the re-scrape-eligible Contracts by ward, keyed by application reference within each
/// ward. Only wards present in this map are ever re-listed (T-24-14).
pub(crate) fn group_by_ward(
    contracts: Vec<UnresolvedContract>,
) -> HashMap<String, HashMap<String, UnresolvedContract>> {
    let mut by_ward: HashMap<String, HashMap<String, UnresolvedContract>> = HashMap::new();
    for c in contracts {
        by_ward
            .entry(c.ward_code.clone())
            .or_default()
            .insert(c.application_ref.clone(), c);
    }
    by_ward
}

/// Whether a freshly-scraped item resolves to a real applicant entity (as opposed to the
/// `Unknown Applicant` fallback). Only `Published` provenance with a non-placeholder slug
/// justifies minting a new entity and edge pair.
pub(crate) fn resolved_applicant(
    item: &PlanningPortalItem,
    provenance: Option<&ApplicantProvenance>,
) -> Option<(String, String)> {
    if provenance != Some(&ApplicantProvenance::Published) {
        return None;
    }
    let name = item.applicant.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let slug = applicant_slug(name);
    if slug.is_empty() {
        return None;
    }
    let entity_id = format!("director:{slug}");
    if entity_id == UNKNOWN_APPLICANT_ID {
        return None;
    }
    Some((entity_id, name.to_owned()))
}

/// Build the mutated Contract weight for a re-scraped record.
///
/// `provenance` is `None` when the re-scrape produced no provenance claim at all (CR-05).
/// This writes through `update_node`, which replaces the row rather than merging it, so a
/// `None` must fall back to whatever was already stored — otherwise a provenance-less
/// re-scrape would blank a measurement this backfill exists to establish.
pub(crate) fn contract_with_provenance(
    data: &ContractData,
    provenance: Option<ApplicantProvenance>,
    detail_handle: Option<String>,
) -> NodeWeight {
    NodeWeight::Contract(ContractData {
        applicant_provenance: provenance.or_else(|| data.applicant_provenance.clone()),
        portal_detail_handle: detail_handle.or_else(|| data.portal_detail_handle.clone()),
        ..data.clone()
    })
}

// ── the mutation/insertion passes ─────────────────────────────────────────────

/// Steps (5), (5a) and (6) for a single ward.
///
/// Ordering mirrors `ward.rs`: new Entity node and new edges are written FIRST, and the Contract's
/// own row is mutated LAST as the commit marker.
/// What to do with a ward after its re-list returns.
#[derive(Debug)]
pub(crate) enum WardAction {
    /// The portal answered. An empty Vec here is a real answer — those references are gone.
    Process(Vec<PlanningPortalItem>),
    /// The re-list failed. We measured nothing, so we must write nothing.
    SkipUnmeasured(ScrapeFailure),
}

/// Decide a ward's fate from its scrape outcome.
///
/// This exists as a pure function because the distinction it draws is the whole fix: before it,
/// `scrape()` flattened a client-side timeout into an empty Vec, which the set-difference branch
/// of `process_ward` reads as "the portal no longer lists these references" and stamps
/// `FetchFailed`. One 180s timeout on ward W05001501 would have overwritten 269 Contracts whose
/// provenance was the stronger, source-verified `AbsentAtSource` (Phase 24 UAT, 2026-07-27).
pub(crate) fn ward_action(outcome: ScrapeOutcome) -> WardAction {
    match outcome {
        ScrapeOutcome::Items(items) => WardAction::Process(items),
        ScrapeOutcome::Failed(failure) => WardAction::SkipUnmeasured(failure),
    }
}

pub(crate) async fn process_ward<O: BackfillOps + ?Sized>(
    ops: &O,
    ward_map: &HashMap<String, UnresolvedContract>,
    scraped: &[PlanningPortalItem],
    inserted_entities: &mut HashSet<String>,
    inserted_locations: &mut HashSet<String>,
    stats: &mut BackfillStats,
    dry_run: bool,
) -> Result<()> {
    let mut seen_refs: HashSet<String> = HashSet::new();

    for item in scraped {
        let Some(reference) = item.reference.as_deref() else {
            continue;
        };
        let Some(contract) = ward_map.get(reference) else {
            continue; // not one of the unresolved records for this ward
        };
        if !seen_refs.insert(reference.to_owned()) {
            continue; // the portal listed the same reference twice
        }

        let provenance = derive_applicant_provenance(item.applicant_provenance.as_deref());
        let updated = contract_with_provenance(
            &contract.data,
            provenance.clone(),
            item.detail_handle.clone(),
        );

        match resolved_applicant(item, provenance.as_ref()) {
            Some((slug_id, applicant_name)) => {
                // ── (5a) edge-existence guard ────────────────────────────────
                // The step-(1) scan selects on the PRESENCE of the placeholder AwardedTo edge,
                // which this backfill deliberately never deletes — so an already-resolved
                // Contract stays in the scan set permanently and a re-run re-selects it.
                // `insert_edge_event` mints a fresh Uuid and issues a plain CREATE: no dedup, no
                // upsert, append-only log. Without this guard, any retry after a mid-run failure
                // (sidecar timeout, SurrealDB transaction conflict — both in this project's
                // history) appends a duplicate AwardedTo + LocatedAt pair per already-resolved
                // Contract, straight into the audit log every coverage figure is computed from.
                if ops.edge_exists(&slug_id, &contract.entity_id).await? {
                    tracing::debug!(
                        contract = %contract.entity_id,
                        applicant = %slug_id,
                        "already resolved by a previous pass — skipping both edge insertions"
                    );
                    stats.already_resolved += 1;
                    // The provenance mutation is naturally idempotent, so it may still run.
                    ops.update_node(&contract.record_id, &updated).await?;
                    continue;
                }

                // ── new Entity node (deduped within the run and against the DB) ──
                if !inserted_entities.contains(&slug_id) {
                    if !ops.entity_exists(&slug_id).await? {
                        ops.insert_node(
                            &slug_id,
                            &NodeWeight::Entity(EntityData {
                                name: applicant_name.clone(),
                                company_number: None,
                                status: EntityStatus::Active,
                                incorporation_date: None,
                            }),
                        )
                        .await?;
                    }
                    inserted_entities.insert(slug_id.clone());
                }

                // ── new AwardedTo + LocatedAt edges (old placeholder edges untouched) ──
                let now = chrono::Utc::now();
                ops.insert_edge(
                    &slug_id,
                    &contract.entity_id,
                    &EdgeWeight::AwardedTo {
                        meta: EdgeMeta {
                            created_at: now,
                            effective_from: None,
                            effective_to: None,
                        },
                        data: AwardedToData {
                            details: format!("backfill:planning application {reference}"),
                        },
                    },
                )
                .await?;

                // ── LocatedAt edge, and the Location node it needs ───────────
                //
                // CR-04: this used to write the edge alone, against a Location node the
                // backfill never created. `GraphStorage::insert_edge` rejects a missing
                // endpoint and WAL replay only `warn!`s and drops it, so the edge lived
                // permanently in the append-only log every coverage figure is computed from
                // while never existing in the graph — and the applicant was silently absent
                // from address clustering, which is the detector criterion 5 relies on.
                //
                // `unwrap_or_default()` on a missing address also produced the literal id
                // `location:`, accumulating a garbage mega-node shared by every addressless
                // record. Mirror the ingest path instead (`mapper::map_planning_item`):
                // require a non-blank address, ensure the node, then write the edge.
                match item
                    .address
                    .as_deref()
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                {
                    Some(address) => {
                        let location_id = location_id_for(address);
                        if !inserted_locations.contains(&location_id.0) {
                            if !ops.entity_exists(&location_id.0).await? {
                                ops.insert_node(
                                    &location_id.0,
                                    &NodeWeight::Location(LocationData {
                                        name: address.to_owned(),
                                        // Same derivation as the ingest path, so a backfilled
                                        // Location is indistinguishable from an ingested one.
                                        postcode: extract_postcode(address),
                                        address_line_1: None,
                                        locality: None,
                                    }),
                                )
                                .await?;
                            }
                            inserted_locations.insert(location_id.0.clone());
                        }
                        ops.insert_edge(
                            &slug_id,
                            &location_id.0,
                            &EdgeWeight::LocatedAt {
                                meta: EdgeMeta {
                                    created_at: now,
                                    effective_from: None,
                                    effective_to: None,
                                },
                                data: LocatedAtData {
                                    details: format!("backfill:{address}"),
                                },
                            },
                        )
                        .await?;
                    }
                    None => {
                        tracing::warn!(
                            contract = %contract.entity_id,
                            applicant = %slug_id,
                            "re-scraped item carries no address — skipping LocatedAt rather than \
                             writing an edge to the empty `location:` id"
                        );
                    }
                }

                // ── commit marker LAST ───────────────────────────────────────
                ops.update_node(&contract.record_id, &updated).await?;
                tracing::info!(
                    contract = %contract.entity_id,
                    applicant = %slug_id,
                    dry_run,
                    "re-attributed planning Contract to a real applicant"
                );
                stats.matched += 1;
            }
            None => {
                // absent_at_source / fetch_failed / no usable applicant name:
                // mutate ONLY the provenance — no new entity, no new edges.
                ops.update_node(&contract.record_id, &updated).await?;
                stats.matched += 1;
            }
        }
    }

    // ── (6) set-difference: records the re-scrape no longer returns at all ──────
    // Never leave these permanently absent — stamp fetch_failed so the corpus reaches full
    // provenance coverage. `skipped` therefore means "provenance corrected to fetch_failed",
    // never a silent no-op.
    for (reference, contract) in ward_map {
        if seen_refs.contains(reference) {
            continue;
        }
        // BL-03: never downgrade a record we already measured as `Published`.
        //
        // This write goes through `update_node_event_by_record`, which replaces the row
        // directly and never passes through `merge_node_weight` — so the CR-05 precedence
        // guard does not apply here and has to be enforced at the call site.
        //
        // Without it the downgrade is not merely possible, it is monotonic. The placeholder
        // `AwardedTo` edge is deliberately never deleted, so an already-resolved Contract is
        // re-selected by the step-(1) scan on every run; and a re-list that windows its
        // results (the Idox strategy queries only the last 365 days) will never return an
        // older reference again. Each run would therefore rewrite `Published` ->
        // `FetchFailed` for those records, degrading the exact figure this phase is graded
        // on, with no failure anywhere to explain it.
        //
        // Absence from a windowed re-list is not evidence that our fetch failed.
        if contract.data.applicant_provenance == Some(ApplicantProvenance::Published) {
            tracing::debug!(
                contract = %contract.entity_id,
                "absent from re-list but already measured as published — leaving provenance intact"
            );
            continue;
        }
        let updated = contract_with_provenance(
            &contract.data,
            Some(ApplicantProvenance::FetchFailed),
            contract.data.portal_detail_handle.clone(),
        );
        ops.update_node(&contract.record_id, &updated).await?;
        tracing::debug!(
            contract = %contract.entity_id,
            "re-scrape no longer returns this reference — stamped fetch_failed"
        );
        stats.skipped += 1;
    }

    Ok(())
}

/// Step (7): mutation-only `Published` stamp. Calls `update_node` and nothing else — never
/// `insert_node`/`insert_edge` (T-24-15), so this pass adds ZERO new `graph_events` rows.
pub(crate) async fn bulk_stamp<O: BackfillOps + ?Sized>(
    ops: &O,
    candidates: &[BulkStampCandidate],
    edge_scoped_ids: &HashSet<String>,
    stats: &mut BackfillStats,
) -> Result<()> {
    for c in candidates {
        // Defence in depth: a Contract reached by the edge-scoped pass is never bulk-stamped,
        // even if the scan that produced `candidates` failed to exclude it.
        if edge_scoped_ids.contains(&c.entity_id) {
            continue;
        }
        if c.data.applicant_provenance.is_some() {
            continue;
        }
        // Provenance is derived from the already-correct AwardedTo edge, not from any re-scrape.
        let updated = contract_with_provenance(
            &c.data,
            Some(ApplicantProvenance::Published),
            c.data.portal_detail_handle.clone(),
        );
        ops.update_node(&c.record_id, &updated).await?;
        stats.bulk_stamped += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── mock ops: records every call so tests can assert on what was NOT called ──

    #[derive(Default)]
    struct MockOps {
        existing_entities: HashSet<String>,
        existing_edges: HashSet<(String, String)>,
        updates: Mutex<Vec<(String, NodeWeight)>>,
        inserted_nodes: Mutex<Vec<String>>,
        inserted_edges: Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl BackfillOps for MockOps {
        async fn entity_exists(&self, entity_id: &str) -> Result<bool> {
            Ok(self.existing_entities.contains(entity_id))
        }
        async fn edge_exists(&self, src_id: &str, dst_id: &str) -> Result<bool> {
            Ok(self
                .existing_edges
                .contains(&(src_id.to_owned(), dst_id.to_owned())))
        }
        async fn update_node(&self, event_id: &str, weight: &NodeWeight) -> Result<()> {
            self.updates
                .lock()
                .unwrap()
                .push((event_id.to_owned(), weight.clone()));
            Ok(())
        }
        async fn insert_node(&self, entity_id: &str, _weight: &NodeWeight) -> Result<()> {
            self.inserted_nodes
                .lock()
                .unwrap()
                .push(entity_id.to_owned());
            Ok(())
        }
        async fn insert_edge(
            &self,
            src_id: &str,
            dst_id: &str,
            _weight: &EdgeWeight,
        ) -> Result<()> {
            self.inserted_edges
                .lock()
                .unwrap()
                .push((src_id.to_owned(), dst_id.to_owned()));
            Ok(())
        }
    }

    impl MockOps {
        fn provenance_for(&self, event_id: &str) -> Option<ApplicantProvenance> {
            self.updates
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(e, _)| e == event_id)
                .and_then(|(_, w)| match w {
                    NodeWeight::Contract(c) => c.applicant_provenance.clone(),
                    _ => None,
                })
        }
        fn edge_insert_count(&self) -> usize {
            self.inserted_edges.lock().unwrap().len()
        }
        fn node_insert_count(&self) -> usize {
            self.inserted_nodes.lock().unwrap().len()
        }
        /// Entity-node inserts only. Since CR-04 the backfill also inserts the `Location`
        /// node behind each `LocatedAt` edge, so a bare total no longer answers "how many
        /// director entities were minted".
        fn entity_insert_count(&self) -> usize {
            self.inserted_nodes
                .lock()
                .unwrap()
                .iter()
                .filter(|id| id.starts_with("director:"))
                .count()
        }
        fn location_insert_count(&self) -> usize {
            self.inserted_nodes
                .lock()
                .unwrap()
                .iter()
                .filter(|id| id.starts_with("location:"))
                .count()
        }
        fn update_count(&self) -> usize {
            self.updates.lock().unwrap().len()
        }
    }

    // ── fixtures ─────────────────────────────────────────────────────────────

    fn contract_data(reference: &str, ward: &str) -> ContractData {
        ContractData {
            name: "Erection of warehouse".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("pending".to_string()),
            date_published: None,
            ward_code: Some(ward.to_string()),
            tender_ref: None,
            application_ref: Some(reference.to_string()),
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        }
    }

    fn unresolved(reference: &str, ward: &str) -> UnresolvedContract {
        UnresolvedContract {
            record_id: format!("graph_events:evt-{reference}"),
            entity_id: format!("planning:{reference}"),
            application_ref: reference.to_string(),
            ward_code: ward.to_string(),
            data: contract_data(reference, ward),
        }
    }

    fn ward_map(contracts: Vec<UnresolvedContract>) -> HashMap<String, UnresolvedContract> {
        contracts
            .into_iter()
            .map(|c| (c.application_ref.clone(), c))
            .collect()
    }

    fn scraped(
        reference: &str,
        applicant: Option<&str>,
        provenance: Option<&str>,
    ) -> PlanningPortalItem {
        PlanningPortalItem {
            reference: Some(reference.to_string()),
            description: Some("Erection of warehouse".to_string()),
            address: Some("1 High St, Llangefni LL77 7AA".to_string()),
            ward: Some("Aethwy".to_string()),
            applicant: applicant.map(str::to_string),
            received_date: None,
            applicant_provenance: provenance.map(str::to_string),
            detail_handle: Some(format!("handle-{reference}")),
        }
    }

    // ── step (5): newly-resolved applicant ───────────────────────────────────

    #[tokio::test]
    async fn test_newly_resolved_applicant_inserts_entity_and_edge_pair() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/001", "W05001492")]);
        let items = vec![scraped(
            "FPL/2024/001",
            Some("Apex Build Ltd"),
            Some("published"),
        )];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(stats.matched, 1);
        assert_eq!(stats.already_resolved, 0);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.bulk_stamped, 0);
        assert_eq!(ops.entity_insert_count(), 1, "one new director entity");
        assert_eq!(
            ops.location_insert_count(),
            1,
            "CR-04: the Location node the LocatedAt edge points at must be created too"
        );
        let edges = ops.inserted_edges.lock().unwrap().clone();
        assert_eq!(edges.len(), 2, "AwardedTo + LocatedAt");
        assert_eq!(
            edges[0],
            (
                "director:apex-build-ltd".to_string(),
                "planning:FPL/2024/001".to_string()
            )
        );
        assert_eq!(edges[1].0, "director:apex-build-ltd");
        assert!(edges[1].1.starts_with("location:"));
        assert_eq!(
            ops.provenance_for("graph_events:evt-FPL/2024/001"),
            Some(ApplicantProvenance::Published)
        );
    }

    #[tokio::test]
    async fn test_existing_entity_is_not_reinserted() {
        let mut ops = MockOps::default();
        ops.existing_entities
            .insert("director:apex-build-ltd".into());
        let map = ward_map(vec![unresolved("FPL/2024/002", "W05001492")]);
        let items = vec![scraped(
            "FPL/2024/002",
            Some("Apex Build Ltd"),
            Some("published"),
        )];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(ops.entity_insert_count(), 0, "entity already exists");
        assert_eq!(ops.edge_insert_count(), 2, "edges are still inserted");
        assert_eq!(stats.matched, 1);
    }

    /// BL-03: a record already measured as `Published` must survive a re-list that no
    /// longer returns it.
    ///
    /// This write bypasses `merge_node_weight` (it goes through `update_node_event_by_record`,
    /// which replaces the row), so the CR-05 precedence guard cannot help — it has to be
    /// enforced at the call site. The failure it prevents is monotonic rather than
    /// occasional: the placeholder edge is never deleted, so resolved Contracts are
    /// re-selected on every run, and a windowed re-list (Idox queries the last 365 days)
    /// never returns older references again. Each run would degrade the phase's headline
    /// number a little further, with no error anywhere to account for it.
    #[tokio::test]
    async fn test_published_is_not_downgraded_when_absent_from_the_relist() {
        let ops = MockOps::default();
        let mut measured = unresolved("FPL/2020/001", "W05001492");
        measured.data.applicant_provenance = Some(ApplicantProvenance::Published);
        let mut unmeasured = unresolved("FPL/2020/002", "W05001492");
        unmeasured.data.applicant_provenance = None;
        let map = ward_map(vec![measured, unmeasured]);

        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        // A re-list that returns neither reference — the windowed-portal case.
        process_ward(
            &ops,
            &map,
            &[],
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        let updates = ops.updates.lock().unwrap().clone();
        let touched: Vec<&String> = updates.iter().map(|(id, _)| id).collect();
        assert!(
            !touched.iter().any(|id| id.contains("FPL/2020/001")),
            "a measured Published record was rewritten by a re-list that simply did not \
             return it — this degrades run over run"
        );
        assert!(
            touched.iter().any(|id| id.contains("FPL/2020/002")),
            "an unmeasured record absent from the re-list should still be stamped fetch_failed"
        );
        assert_eq!(
            stats.skipped, 1,
            "only the unmeasured record counts as skipped"
        );
    }

    /// CR-04: every `LocatedAt` edge must have a `Location` node to point at.
    ///
    /// The backfill used to write the edge alone. `GraphStorage::insert_edge` rejects a
    /// missing endpoint and WAL replay only `warn!`s and drops it, so the edge sat
    /// permanently in the append-only log — which every coverage figure is computed from —
    /// while never existing in the graph, and the re-attributed applicant was silently
    /// excluded from address clustering.
    #[tokio::test]
    async fn test_located_at_edge_always_has_its_location_node() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/050", "W05001492")]);
        let items = vec![scraped(
            "FPL/2024/050",
            Some("Apex Build Ltd"),
            Some("published"),
        )];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        let nodes = ops.inserted_nodes.lock().unwrap().clone();
        let edges = ops.inserted_edges.lock().unwrap().clone();
        for (_, dst) in edges.iter().filter(|(_, d)| d.starts_with("location:")) {
            assert!(
                nodes.contains(dst),
                "LocatedAt edge points at {dst}, which was never created — replay would drop \
                 the edge while it stays in the audit log forever"
            );
        }
    }

    /// CR-04: an addressless record used to produce `unwrap_or_default()` -> the literal id
    /// `location:`, a garbage mega-node shared by every such record across the corpus.
    #[tokio::test]
    async fn test_addressless_item_writes_no_location_edge_or_node() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/051", "W05001492")]);
        let mut item = scraped("FPL/2024/051", Some("Apex Build Ltd"), Some("published"));
        item.address = None;
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &[item],
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        let nodes = ops.inserted_nodes.lock().unwrap().clone();
        let edges = ops.inserted_edges.lock().unwrap().clone();
        assert!(
            !nodes.iter().any(|n| n == "location:"),
            "the empty address produced the `location:` mega-node"
        );
        assert!(
            !edges.iter().any(|(_, d)| d == "location:"),
            "an edge was written to the empty `location:` id"
        );
        // The attribution itself must still happen — only the LocatedAt edge is skipped.
        assert_eq!(stats.matched, 1);
        assert_eq!(
            edges.len(),
            1,
            "AwardedTo only; LocatedAt is skipped without an address"
        );
    }

    /// CR-04: a whitespace-only address is as useless as a missing one and must not mint
    /// `location:` or a whitespace-slug node either.
    #[tokio::test]
    async fn test_blank_address_is_treated_as_no_address() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/052", "W05001492")]);
        let mut item = scraped("FPL/2024/052", Some("Apex Build Ltd"), Some("published"));
        item.address = Some("   ".to_string());
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &[item],
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(ops.location_insert_count(), 0);
        let edges = ops.inserted_edges.lock().unwrap().clone();
        assert!(!edges.iter().any(|(_, d)| d.starts_with("location:")));
    }

    #[tokio::test]
    async fn test_entity_inserted_once_across_contracts_sharing_an_applicant() {
        let ops = MockOps::default();
        let map = ward_map(vec![
            unresolved("FPL/2024/003", "W05001492"),
            unresolved("FPL/2024/004", "W05001492"),
        ]);
        let items = vec![
            scraped("FPL/2024/003", Some("Apex Build Ltd"), Some("published")),
            scraped("FPL/2024/004", Some("Apex Build Ltd"), Some("published")),
        ];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            ops.entity_insert_count(),
            1,
            "entity deduped within the run"
        );
        assert_eq!(
            ops.location_insert_count(),
            1,
            "both contracts share one address, so the Location node is written once"
        );
        assert_eq!(ops.edge_insert_count(), 4, "two edge pairs");
        assert_eq!(stats.matched, 2);
    }

    // ── step (5a): the re-entrancy guard ─────────────────────────────────────

    /// REGRESSION GATE for the step-(5a) edge-existence guard.
    ///
    /// The placeholder `AwardedTo` edge is deliberately never deleted, so an already-resolved
    /// Contract stays in the edge-scoped scan set forever and a re-run re-selects it.
    /// `insert_edge_event` has no dedup or upsert and `graph_events` is append-only, so without
    /// the guard a retry appends a duplicate AwardedTo + LocatedAt pair into the audit log every
    /// coverage figure is computed from. THIS TEST MUST FAIL IF THE GUARD IS REMOVED.
    #[tokio::test]
    async fn test_reentrancy_guard_inserts_zero_edges_for_already_resolved_contract() {
        let mut ops = MockOps::default();
        // A previous pass already inserted the real AwardedTo edge...
        ops.existing_edges.insert((
            "director:apex-build-ltd".to_string(),
            "planning:FPL/2024/005".to_string(),
        ));
        ops.existing_entities
            .insert("director:apex-build-ltd".into());
        // ...but the placeholder edge was never deleted, so the Contract is still in the scan set.
        let map = ward_map(vec![unresolved("FPL/2024/005", "W05001492")]);
        let items = vec![scraped(
            "FPL/2024/005",
            Some("Apex Build Ltd"),
            Some("published"),
        )];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            ops.edge_insert_count(),
            0,
            "a re-run must add ZERO new EdgeAdded rows for an already-resolved Contract"
        );
        assert_eq!(ops.entity_insert_count(), 0, "no new entity either");
        assert_eq!(stats.already_resolved, 1);
        assert_eq!(
            stats.matched, 0,
            "already_resolved is not counted as matched"
        );
        // The provenance mutation is idempotent and may still run.
        assert_eq!(
            ops.provenance_for("graph_events:evt-FPL/2024/005"),
            Some(ApplicantProvenance::Published)
        );
    }

    /// A full second pass over the same input must be a no-op for edge insertion.
    #[tokio::test]
    async fn test_second_pass_over_same_input_adds_no_edges() {
        let mut ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/006", "W05001492")]);
        let items = vec![scraped(
            "FPL/2024/006",
            Some("Apex Build Ltd"),
            Some("published"),
        )];

        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();
        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();
        assert_eq!(ops.edge_insert_count(), 2);

        // Simulate the first pass's writes landing in the DB, then re-run.
        let written = ops.inserted_edges.lock().unwrap().clone();
        for (src, dst) in written {
            ops.existing_edges.insert((src, dst));
        }
        ops.existing_entities
            .insert("director:apex-build-ltd".into());
        ops.inserted_edges.lock().unwrap().clear();
        ops.inserted_nodes.lock().unwrap().clear();

        let mut inserted2 = HashSet::new();
        let mut locations2 = HashSet::new();
        let mut stats2 = BackfillStats::default();
        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted2,
            &mut locations2,
            &mut stats2,
            false,
        )
        .await
        .unwrap();

        assert_eq!(ops.edge_insert_count(), 0, "re-run must add zero edges");
        assert_eq!(stats2.already_resolved, 1);
    }

    // ── step (5): unresolved re-scrape outcomes ──────────────────────────────

    #[tokio::test]
    async fn test_absent_at_source_mutates_provenance_only() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/007", "W05001492")]);
        let items = vec![scraped("FPL/2024/007", None, Some("absent_at_source"))];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(ops.edge_insert_count(), 0);
        assert_eq!(ops.node_insert_count(), 0);
        assert_eq!(stats.matched, 1);
        assert_eq!(
            ops.provenance_for("graph_events:evt-FPL/2024/007"),
            Some(ApplicantProvenance::AbsentAtSource)
        );
    }

    #[tokio::test]
    async fn test_rescraped_fetch_failed_mutates_provenance_only() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/008", "W05001492")]);
        let items = vec![scraped("FPL/2024/008", None, Some("fetch_failed"))];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(ops.edge_insert_count(), 0);
        assert_eq!(stats.matched, 1);
        assert_eq!(
            ops.provenance_for("graph_events:evt-FPL/2024/008"),
            Some(ApplicantProvenance::FetchFailed)
        );
    }

    /// A "published" provenance whose applicant name is still the Unknown-Applicant placeholder
    /// must never mint `director:unknown-applicant` edges.
    #[tokio::test]
    async fn test_published_but_unknown_applicant_name_inserts_no_edges() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/009", "W05001492")]);
        let items = vec![scraped(
            "FPL/2024/009",
            Some("Unknown Applicant"),
            Some("published"),
        )];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(ops.edge_insert_count(), 0);
        assert_eq!(ops.node_insert_count(), 0);
        assert_eq!(stats.matched, 1);
    }

    // ── step (6): the re-scrape no longer returns this reference ─────────────

    /// The `skipped` path must be a real `update_node` call carrying `FetchFailed`, never a
    /// silent no-op — otherwise the record's provenance stays permanently absent and the
    /// probe's `not_yet_measured` bucket can never reach zero.
    // ── the ward-outcome branch (regression gate for the UAT finding) ────────

    #[test]
    fn test_ward_action_separates_a_failed_relist_from_an_empty_one() {
        match ward_action(ScrapeOutcome::Items(vec![])) {
            WardAction::Process(items) => assert!(items.is_empty()),
            other => panic!("an empty-but-successful re-list is an answer: {other:?}"),
        }
        for failure in [
            ScrapeFailure::Transport,
            ScrapeFailure::ErrorStatus,
            ScrapeFailure::MalformedResponse,
        ] {
            match ward_action(ScrapeOutcome::Failed(failure)) {
                WardAction::SkipUnmeasured(f) => assert_eq!(f, failure),
                other => panic!("{failure:?} must not be processed as data: {other:?}"),
            }
        }
    }

    /// The defect this whole change exists for: ward W05001501 timed out at the hardcoded
    /// 180s ceiling during the Phase 24 UAT run, `scrape()` flattened that to `vec![]`, and
    /// the set-difference branch would have stamped all 269 of its Contracts `FetchFailed` —
    /// overwriting the stronger, source-verified `AbsentAtSource`.
    ///
    /// Reproduces both halves of the branch against the same ward map, so the assertion is
    /// about the DIFFERENCE, not about one arm in isolation.
    #[tokio::test]
    async fn test_failed_relist_writes_nothing_while_empty_relist_stamps_fetch_failed() {
        let map = ward_map(vec![
            unresolved("FPL/2024/010", "W05001501"),
            unresolved("FPL/2024/011", "W05001501"),
        ]);

        // (a) transport failure → not a single write of any kind
        let failed_ops = MockOps::default();
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut failed_stats = BackfillStats::default();
        match ward_action(ScrapeOutcome::Failed(ScrapeFailure::Transport)) {
            WardAction::Process(items) => {
                process_ward(
                    &failed_ops,
                    &map,
                    &items,
                    &mut inserted,
                    &mut locations,
                    &mut failed_stats,
                    false,
                )
                .await
                .unwrap();
            }
            WardAction::SkipUnmeasured(_) => {
                failed_stats.errors += map.len();
            }
        }
        assert_eq!(
            failed_ops.update_count(),
            0,
            "a failed re-list must not write provenance — we measured nothing"
        );
        assert_eq!(failed_ops.edge_insert_count(), 0);
        assert_eq!(failed_ops.node_insert_count(), 0);
        assert_eq!(failed_stats.skipped, 0, "nothing was skipped; nothing ran");
        assert_eq!(
            failed_stats.errors, 2,
            "every unmeasured Contract in the ward is counted as an error"
        );

        // (b) the portal genuinely answered with nothing → stamp fetch_failed as before
        let empty_ops = MockOps::default();
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut empty_stats = BackfillStats::default();
        match ward_action(ScrapeOutcome::Items(vec![])) {
            WardAction::Process(items) => {
                process_ward(
                    &empty_ops,
                    &map,
                    &items,
                    &mut inserted,
                    &mut locations,
                    &mut empty_stats,
                    false,
                )
                .await
                .unwrap();
            }
            WardAction::SkipUnmeasured(_) => panic!("an empty answer is still an answer"),
        }
        assert_eq!(empty_stats.skipped, 2);
        assert_eq!(
            empty_ops.provenance_for("graph_events:evt-FPL/2024/010"),
            Some(ApplicantProvenance::FetchFailed),
            "a genuinely delisted record is still stamped fetch_failed"
        );
    }

    #[tokio::test]
    async fn test_reference_absent_from_rescrape_is_stamped_fetch_failed() {
        let ops = MockOps::default();
        let map = ward_map(vec![
            unresolved("FPL/2024/010", "W05001492"),
            unresolved("FPL/2024/011", "W05001492"), // delisted since the original ingest
        ]);
        let items = vec![scraped(
            "FPL/2024/010",
            Some("Apex Build Ltd"),
            Some("published"),
        )];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(stats.matched, 1);
        assert_eq!(stats.skipped, 1);
        assert_eq!(
            ops.provenance_for("graph_events:evt-FPL/2024/011"),
            Some(ApplicantProvenance::FetchFailed),
            "a delisted record must be stamped fetch_failed, not left absent"
        );
        // No entity/edge writes for the delisted record.
        assert_eq!(ops.edge_insert_count(), 2, "only the matched record's pair");
    }

    #[tokio::test]
    async fn test_empty_rescrape_stamps_whole_ward_fetch_failed() {
        let ops = MockOps::default();
        let map = ward_map(vec![
            unresolved("FPL/2024/012", "W05001492"),
            unresolved("FPL/2024/013", "W05001492"),
        ]);
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &[],
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(stats.skipped, 2);
        assert_eq!(stats.matched, 0);
        assert_eq!(ops.edge_insert_count(), 0);
        assert_eq!(ops.update_count(), 2);
    }

    /// A scraped reference that is not one of this ward's unresolved records is ignored —
    /// the re-list returns the whole ward, but only the unresolved slice is acted on.
    #[tokio::test]
    async fn test_unrelated_scraped_reference_is_ignored() {
        let ops = MockOps::default();
        let map = ward_map(vec![unresolved("FPL/2024/014", "W05001492")]);
        let items = vec![
            scraped("FPL/2024/014", Some("Apex Build Ltd"), Some("published")),
            scraped("FPL/2024/999", Some("Other Ltd"), Some("published")),
        ];
        let mut inserted = HashSet::new();
        let mut locations = HashSet::new();
        let mut stats = BackfillStats::default();

        process_ward(
            &ops,
            &map,
            &items,
            &mut inserted,
            &mut locations,
            &mut stats,
            false,
        )
        .await
        .unwrap();

        assert_eq!(stats.matched, 1);
        assert_eq!(stats.skipped, 0);
        assert_eq!(ops.update_count(), 1);
    }

    // ── step (7): the bulk stamp ─────────────────────────────────────────────

    /// T-24-15: the bulk-stamp branch is a PURE `update_node` mutation. A Contract not present in
    /// the edge-scoped map is stamped `Published` and must NEVER receive an edge or node insert.
    #[tokio::test]
    async fn test_bulk_stamp_publishes_and_never_inserts_an_edge() {
        let ops = MockOps::default();
        let edge_scoped: HashSet<String> =
            ["planning:FPL/2024/020".to_string()].into_iter().collect();
        let candidates = vec![BulkStampCandidate {
            record_id: "graph_events:evt-FPL/2024/021".to_string(),
            entity_id: "planning:FPL/2024/021".to_string(),
            data: contract_data("FPL/2024/021", "W05001492"),
        }];
        let mut stats = BackfillStats::default();

        bulk_stamp(&ops, &candidates, &edge_scoped, &mut stats)
            .await
            .unwrap();

        assert_eq!(stats.bulk_stamped, 1);
        assert_eq!(
            ops.edge_insert_count(),
            0,
            "the bulk-stamp pass must never call insert_edge"
        );
        assert_eq!(
            ops.node_insert_count(),
            0,
            "the bulk-stamp pass must never call insert_node"
        );
        assert_eq!(
            ops.provenance_for("graph_events:evt-FPL/2024/021"),
            Some(ApplicantProvenance::Published)
        );
    }

    /// A Contract reached by the edge-scoped pass is never bulk-stamped, even if it leaks into
    /// the candidate list.
    #[tokio::test]
    async fn test_bulk_stamp_excludes_edge_scoped_contracts() {
        let ops = MockOps::default();
        let edge_scoped: HashSet<String> =
            ["planning:FPL/2024/022".to_string()].into_iter().collect();
        let candidates = vec![BulkStampCandidate {
            record_id: "graph_events:evt-FPL/2024/022".to_string(),
            entity_id: "planning:FPL/2024/022".to_string(),
            data: contract_data("FPL/2024/022", "W05001492"),
        }];
        let mut stats = BackfillStats::default();

        bulk_stamp(&ops, &candidates, &edge_scoped, &mut stats)
            .await
            .unwrap();

        assert_eq!(stats.bulk_stamped, 0);
        assert_eq!(ops.update_count(), 0);
    }

    #[tokio::test]
    async fn test_bulk_stamp_skips_contracts_that_already_have_provenance() {
        let ops = MockOps::default();
        let mut data = contract_data("FPL/2024/023", "W05001492");
        data.applicant_provenance = Some(ApplicantProvenance::AbsentAtSource);
        let candidates = vec![BulkStampCandidate {
            record_id: "graph_events:evt-FPL/2024/023".to_string(),
            entity_id: "planning:FPL/2024/023".to_string(),
            data,
        }];
        let mut stats = BackfillStats::default();

        bulk_stamp(&ops, &candidates, &HashSet::new(), &mut stats)
            .await
            .unwrap();

        assert_eq!(stats.bulk_stamped, 0);
        assert_eq!(
            ops.update_count(),
            0,
            "existing provenance is not overwritten"
        );
    }

    // ── pure helpers ─────────────────────────────────────────────────────────

    #[test]
    fn test_application_ref_prefers_persisted_value() {
        let data = contract_data("FPL/2024/030", "W05001492");
        assert_eq!(
            application_ref_for("planning:FPL/2024/030", &data),
            Some("FPL/2024/030".to_string())
        );
    }

    #[test]
    fn test_application_ref_falls_back_to_entity_id() {
        let mut data = contract_data("FPL/2024/031", "W05001492");
        data.application_ref = None;
        assert_eq!(
            application_ref_for("planning:FPL/2024/031", &data),
            Some("FPL/2024/031".to_string())
        );
    }

    #[test]
    fn test_application_ref_none_for_non_planning_entity() {
        let mut data = contract_data("x", "W05001492");
        data.application_ref = None;
        assert_eq!(application_ref_for("CF:abc123", &data), None);
    }

    #[test]
    fn test_group_by_ward_partitions_and_keys_by_reference() {
        let by_ward = group_by_ward(vec![
            unresolved("A/1", "W05001492"),
            unresolved("A/2", "W05001492"),
            unresolved("B/1", "W05001493"),
        ]);
        assert_eq!(by_ward.len(), 2);
        assert_eq!(by_ward["W05001492"].len(), 2);
        assert!(by_ward["W05001492"].contains_key("A/1"));
        assert_eq!(by_ward["W05001493"].len(), 1);
    }

    #[test]
    fn test_resolved_applicant_requires_published_provenance() {
        let item = scraped("A/1", Some("Apex Build Ltd"), Some("absent_at_source"));
        assert!(resolved_applicant(&item, Some(&ApplicantProvenance::AbsentAtSource)).is_none());
        assert_eq!(
            resolved_applicant(&item, Some(&ApplicantProvenance::Published)),
            Some((
                "director:apex-build-ltd".to_string(),
                "Apex Build Ltd".to_string()
            ))
        );
    }

    /// CR-05: an unmeasured record (no provenance claim at all) must never mint an entity
    /// or an edge — only an affirmative `Published` justifies that.
    #[test]
    fn test_resolved_applicant_rejects_unmeasured_provenance() {
        let item = scraped("A/1", Some("Apex Build Ltd"), None);
        assert!(resolved_applicant(&item, None).is_none());
    }

    #[test]
    fn test_resolved_applicant_rejects_placeholder_and_blank_names() {
        let unknown = scraped("A/1", Some("Unknown Applicant"), Some("published"));
        assert!(resolved_applicant(&unknown, Some(&ApplicantProvenance::Published)).is_none());
        let blank = scraped("A/2", Some("   "), Some("published"));
        assert!(resolved_applicant(&blank, Some(&ApplicantProvenance::Published)).is_none());
        let punct = scraped("A/3", Some("---"), Some("published"));
        assert!(resolved_applicant(&punct, Some(&ApplicantProvenance::Published)).is_none());
        let missing = scraped("A/4", None, Some("published"));
        assert!(resolved_applicant(&missing, Some(&ApplicantProvenance::Published)).is_none());
    }

    #[test]
    fn test_contract_with_provenance_preserves_other_fields_and_takes_new_handle() {
        let data = contract_data("FPL/2024/040", "W05001492");
        let w = contract_with_provenance(
            &data,
            Some(ApplicantProvenance::Published),
            Some("handle-x".to_string()),
        );
        match w {
            NodeWeight::Contract(c) => {
                assert_eq!(c.applicant_provenance, Some(ApplicantProvenance::Published));
                assert_eq!(c.portal_detail_handle.as_deref(), Some("handle-x"));
                assert_eq!(c.application_ref.as_deref(), Some("FPL/2024/040"));
                assert_eq!(c.ward_code.as_deref(), Some("W05001492"));
                assert_eq!(c.name, "Erection of warehouse");
            }
            _ => panic!("expected a Contract weight"),
        }
    }

    #[test]
    fn test_contract_with_provenance_keeps_existing_handle_when_none_supplied() {
        let mut data = contract_data("FPL/2024/041", "W05001492");
        data.portal_detail_handle = Some("existing".to_string());
        let w = contract_with_provenance(&data, Some(ApplicantProvenance::FetchFailed), None);
        match w {
            NodeWeight::Contract(c) => {
                assert_eq!(c.portal_detail_handle.as_deref(), Some("existing"));
                assert_eq!(
                    c.applicant_provenance,
                    Some(ApplicantProvenance::FetchFailed)
                );
            }
            _ => panic!("expected a Contract weight"),
        }
    }

    /// CR-05: `update_node` replaces the row rather than merging it, so a re-scrape that
    /// returns no provenance claim must fall back to the stored value. Writing `None`
    /// through would blank a measurement this backfill exists to establish — and because
    /// the write is a plain row mutation, nothing downstream would notice.
    #[test]
    fn test_contract_with_provenance_keeps_stored_provenance_when_unmeasured() {
        let mut data = contract_data("FPL/2024/042", "W05001492");
        data.applicant_provenance = Some(ApplicantProvenance::Published);
        let w = contract_with_provenance(&data, None, None);
        match w {
            NodeWeight::Contract(c) => {
                assert_eq!(
                    c.applicant_provenance,
                    Some(ApplicantProvenance::Published),
                    "an unmeasured re-scrape must not erase a stored measurement"
                );
            }
            _ => panic!("expected a Contract weight"),
        }
    }

    #[test]
    fn test_bulk_stamp_candidates_excludes_edge_scoped_and_already_stamped() {
        let mut latest: HashMap<String, (String, ContractData)> = HashMap::new();
        latest.insert(
            "planning:A".to_string(),
            ("graph_events:a".to_string(), contract_data("A", "W1")),
        );
        latest.insert(
            "planning:B".to_string(),
            ("graph_events:b".to_string(), contract_data("B", "W1")),
        );
        let mut stamped = contract_data("C", "W1");
        stamped.applicant_provenance = Some(ApplicantProvenance::Published);
        latest.insert(
            "planning:C".to_string(),
            ("graph_events:c".to_string(), stamped),
        );

        let edge_scoped: HashSet<String> = ["planning:A".to_string()].into_iter().collect();
        let out = bulk_stamp_candidates(&latest, &edge_scoped);

        let ids: HashSet<&str> = out.iter().map(|c| c.entity_id.as_str()).collect();
        assert_eq!(
            ids,
            ["planning:B"].into_iter().collect::<HashSet<&str>>(),
            "A is edge-scoped (re-scrape path owns it) and C already has provenance; only B is a \
             bulk-stamp candidate"
        );
        assert_eq!(out[0].record_id, "graph_events:b");
    }

    #[test]
    fn test_unknown_applicant_id_matches_the_mapper_slug() {
        // Regression guard: the scan gate must stay in lockstep with map_planning_item's
        // "Unknown Applicant" fallback slug.
        assert_eq!(
            format!("director:{}", applicant_slug("Unknown Applicant")),
            UNKNOWN_APPLICANT_ID
        );
    }

    // ── DB-gated: the existence checks must work against a REAL SurrealDB ──────
    //
    // Every test above this line drives `MockOps`, so none of them ever sends
    // `SurrealOps`' SurrealQL to a database. That blind spot shipped a dead
    // re-entrancy guard: with `kind = 'EdgeAdded'` in the WHERE clause alongside an
    // equality on an unindexed field, SurrealDB 3.x took the `idx_graph_events_kind`
    // path and returned an EMPTY row set while `count()` on the identical predicate
    // returned 1 — so `edge_exists` answered `false` for every live row and the guard
    // that exists to keep duplicate edges out of the append-only log never fired.
    //
    // These tests are the only thing that can catch that class of defect, and CI runs
    // them (`URBAN_DB_TESTS=1`, per .github/workflows/ci.yml).

    fn db_tests_enabled() -> bool {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return false;
        }
        std::net::TcpStream::connect("[::1]:8000").is_ok()
            || std::net::TcpStream::connect("127.0.0.1:8000").is_ok()
    }

    /// Returns the session plus the namespace guard; callers must bind the
    /// guard so the namespace outlives the test.
    async fn connect_scratch_db(
        suffix: &str,
    ) -> (DbSession, urban_integrity_test_support::NsGuard) {
        let guard = urban_integrity_test_support::NsGuard::with_prefix("test_backfill_planning");
        use std::sync::Arc;
        use surrealdb::engine::any::Any;
        use surrealdb::opt::auth::Root;
        use surrealdb::Surreal;
        let db: Surreal<Any> = Surreal::init();
        db.connect("http://localhost:8000").await.expect("connect");
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await
        .expect("signin");
        // Retry on write conflict — parallel tests race to create the namespace. See
        // `use_ns_db_with_retry`. Latent here since this helper was written; it only
        // fires from a clean SurrealDB, so CI hit it while local runs did not.
        crate::backfill::use_ns_db_with_retry(&db, guard.name(), &format!("existence_{suffix}"))
            .await;
        // `kind` must be indexed for this to be a faithful reproduction — the defect
        // only appears when the planner has an index available to take.
        db.query("DEFINE INDEX IF NOT EXISTS idx_graph_events_kind ON graph_events FIELDS kind")
            .await
            .expect("define index");
        let session = DbSession::new(
            Arc::new(db),
            crate::backfill::SessionCreds {
                username: "root".to_string(),
                password: "root".to_string(),
                namespace: guard.name().to_string(),
                database: format!("existence_{suffix}"),
            },
        );
        (session, guard)
    }

    #[tokio::test]
    async fn db_edge_exists_finds_a_real_edge_row() {
        if !db_tests_enabled() {
            eprintln!("SKIP: db_edge_exists_finds_a_real_edge_row (set URBAN_DB_TESTS=1)");
            return;
        }
        let (db, _ns) = connect_scratch_db("edge").await;
        db.handle()
            .query("DELETE graph_events")
            .await
            .expect("clean");
        db.handle()
            .query(
                "CREATE graph_events SET kind = 'EdgeAdded', event_id = 'e1', timestamp = 1, \
             payload = { src_id: 'director:jane-doe', dst_id: 'planning:ABC1' }",
            )
            .await
            .expect("seed edge");

        let ops = SurrealOps {
            session: db.clone(),
            dry_run: true,
        };

        assert!(
            ops.edge_exists("director:jane-doe", "planning:ABC1")
                .await
                .expect("edge_exists"),
            "edge_exists returned false for an edge that IS present — the re-entrancy guard is \
             dead and a re-run will append duplicate edges to the append-only log"
        );
        assert!(
            !ops.edge_exists("director:jane-doe", "planning:NOPE")
                .await
                .expect("edge_exists"),
            "edge_exists must not report a match for an absent edge"
        );
    }

    #[tokio::test]
    async fn db_update_by_record_id_writes_the_row() {
        if !db_tests_enabled() {
            eprintln!("SKIP: db_update_by_record_id_writes_the_row (set URBAN_DB_TESTS=1)");
            return;
        }
        let (db, _ns) = connect_scratch_db("update").await;
        db.handle()
            .query("DELETE graph_events")
            .await
            .expect("clean");
        db.handle()
            .query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = 'n1', timestamp = 1, \
             payload = { entity_id: 'planning:ABC1', weight_json: 'old' }",
            )
            .await
            .expect("seed");

        let rows: Vec<serde_json::Value> = db
            .handle()
            .query("SELECT id FROM graph_events LIMIT 1")
            .await
            .expect("select id")
            .take(0)
            .expect("take");
        let record_id = extract_id_str(&rows[0]["id"]).expect("record id");

        let weight = NodeWeight::Contract(contract_data("ABC1", "W05001492"));
        update_node_event_by_record(&db, &record_id, &weight)
            .await
            .expect("update by record id must succeed");

        let after: Vec<serde_json::Value> = db
            .handle()
            .query("SELECT payload.weight_json AS w FROM graph_events LIMIT 1")
            .await
            .expect("re-read")
            .take(0)
            .expect("take");
        let written = after[0]["w"].as_str().expect("weight_json");
        assert!(
            written.contains("\"type\":\"Contract\""),
            "primary-key update did not write the new weight_json (got {written:?})"
        );

        // A record id that does not exist must fail loud, never silently no-op — the ICIJ
        // adapter incident (10h of silent no-op writes) is why this is asserted.
        assert!(
            update_node_event_by_record(&db, "graph_events:doesnotexist", &weight)
                .await
                .is_err(),
            "update against a missing record id must return Err, not Ok"
        );
    }

    /// A record id whose key needs quoting must still resolve.
    ///
    /// SurrealDB returns record ids backtick-quoted when the key contains characters that
    /// are not bare-identifier-safe — a hyphen is enough. Every real Contracts Finder
    /// contract key is of the form `NodeAdded_CF_ocds-b5fd17-0010f7d4-…`, so every one of
    /// them comes back as ``graph_events:`NodeAdded_CF_ocds-…` ``. Binding that key
    /// verbatim to `type::record()` looks up a key that literally begins with a backtick,
    /// which matches nothing, and `backfill ward` aborted on its first contract.
    ///
    /// The pre-existing test above could not catch this: it lets SurrealDB generate the
    /// key, and a generated key is always bare-safe and therefore never quoted.
    #[tokio::test]
    async fn db_update_by_record_id_handles_a_quoted_key() {
        if !db_tests_enabled() {
            eprintln!("SKIP: db_update_by_record_id_handles_a_quoted_key (set URBAN_DB_TESTS=1)");
            return;
        }
        let (db, _ns) = connect_scratch_db("quotedkey").await;
        db.handle()
            .query("DELETE graph_events")
            .await
            .expect("clean");
        // The real shape, hyphens and all.
        db.handle()
            .query(
                "CREATE graph_events:`NodeAdded_CF_ocds-b5fd17-0010f7d4-4b63` SET \
             kind = 'NodeAdded', event_id = 'n1', timestamp = 1, \
             payload = { entity_id: 'CF:ocds-b5fd17-0010f7d4-4b63', weight_json: 'old' }",
            )
            .await
            .expect("seed");

        let rows: Vec<serde_json::Value> = db
            .handle()
            .query("SELECT id FROM graph_events LIMIT 1")
            .await
            .expect("select id")
            .take(0)
            .expect("take");
        let record_id = extract_id_str(&rows[0]["id"]).expect("record id");
        assert!(
            record_id.contains('`'),
            "precondition: SurrealDB must return this key backtick-quoted, got {record_id:?}"
        );

        let weight = NodeWeight::Contract(contract_data("ABC1", "W05001492"));
        update_node_event_by_record(&db, &record_id, &weight)
            .await
            .expect("update must resolve a quoted record id");

        let after: Vec<serde_json::Value> = db
            .handle()
            .query("SELECT payload.weight_json AS w FROM graph_events LIMIT 1")
            .await
            .expect("re-read")
            .take(0)
            .expect("take");
        let written = after[0]["w"].as_str().expect("weight_json");
        assert!(
            written.contains("\"type\":\"Contract\""),
            "quoted-key update did not write the new weight_json (got {written:?})"
        );

        // Stripping the quotes must not turn a genuinely-absent record into a silent no-op.
        assert!(
            update_node_event_by_record(&db, "graph_events:`NodeAdded_CF_nope-nope`", &weight)
                .await
                .is_err(),
            "a quoted record id that does not exist must still return Err"
        );
    }

    #[tokio::test]
    async fn db_entity_exists_finds_a_real_node_row() {
        if !db_tests_enabled() {
            eprintln!("SKIP: db_entity_exists_finds_a_real_node_row (set URBAN_DB_TESTS=1)");
            return;
        }
        let (db, _ns) = connect_scratch_db("entity").await;
        db.handle()
            .query("DELETE graph_events")
            .await
            .expect("clean");
        db.handle()
            .query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = 'n1', timestamp = 1, \
             payload = { entity_id: 'director:jane-doe' }",
            )
            .await
            .expect("seed node");
        // An EdgeAdded row carrying the same id must NOT be mistaken for the node —
        // this is why `kind` is filtered Rust-side rather than simply dropped.
        db.handle()
            .query(
                "CREATE graph_events SET kind = 'EdgeAdded', event_id = 'e9', timestamp = 2, \
             payload = { entity_id: 'director:only-an-edge' }",
            )
            .await
            .expect("seed edge");

        let ops = SurrealOps {
            session: db.clone(),
            dry_run: true,
        };

        assert!(
            ops.entity_exists("director:jane-doe")
                .await
                .expect("entity_exists"),
            "entity_exists returned false for a node that IS present — a duplicate entity node \
             would be inserted on every re-run"
        );
        assert!(
            !ops.entity_exists("director:only-an-edge")
                .await
                .expect("entity_exists"),
            "entity_exists must not match a non-NodeAdded row that happens to carry entity_id"
        );
    }

    /// CR-06 regression. The old implementation used `LIMIT 8` with no `ORDER BY`, so an
    /// entity with more than 8 events could return an arbitrary all-`NodeUpdated` subset and
    /// answer `false` despite having a `NodeAdded` row. Enrichment writes many `NodeUpdated`
    /// rows per entity (status transitions, Gazette InLiquidation, CH merges), so this is the
    /// normal state of an enriched entity, not a corner case.
    ///
    /// The consequence being guarded against is not a duplicate row: the caller responds to a
    /// false `false` by inserting a BARE `EntityData` (company_number `None`, status `Active`)
    /// stamped `now()`, which wins latest-wins replay — resurrecting a Dissolved company and
    /// silently disabling the Phoenix detector for it.
    ///
    /// Seeds the `NodeAdded` row LAST so any implementation that stops after a fixed prefix
    /// of the id order fails this test.
    #[tokio::test]
    async fn db_entity_exists_survives_many_node_updated_rows() {
        if !db_tests_enabled() {
            eprintln!(
                "SKIP: db_entity_exists_survives_many_node_updated_rows (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let (db, _ns) = connect_scratch_db("entity_paging").await;
        db.handle()
            .query("DELETE graph_events")
            .await
            .expect("clean");

        // 40 NodeUpdated rows — comfortably past the old LIMIT 8, and past any single small
        // page — all carrying the same entity_id, exactly as enrichment writes them.
        for i in 0..40 {
            db.handle()
                .query(format!(
                    "CREATE graph_events SET kind = 'NodeUpdated', event_id = 'u{i}', \
                 timestamp = {i}, payload = {{ entity_id: 'director:heavily-enriched' }}"
                ))
                .await
                .expect("seed NodeUpdated");
        }
        db.handle()
            .query(
                "CREATE graph_events SET kind = 'NodeAdded', event_id = 'zzz-node', \
             timestamp = 99, payload = { entity_id: 'director:heavily-enriched' }",
            )
            .await
            .expect("seed NodeAdded");

        let ops = SurrealOps {
            session: db.clone(),
            dry_run: true,
        };

        assert!(
            ops.entity_exists("director:heavily-enriched")
                .await
                .expect("entity_exists"),
            "entity_exists missed a NodeAdded row behind 40 NodeUpdated rows — the caller would \
             now insert a bare Active entity that overwrites the enriched one at replay"
        );
    }
}
