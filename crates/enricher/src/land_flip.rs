//! Pre-Permission Land Flip detector (FLIP-02/03/04/05).
//!
//! This is the phase's central, security-critical deliverable. For each in-scope
//! planning application it runs THREE mandatory gates against staged PPD transactions:
//!
//! 1. **gate-1 (structural address)** — `postcode_norm` AND `paon_norm` AND `street_norm`
//!    must all match. A postcode-only match produces ZERO candidates (FLIP-03).
//! 2. **gate-2 (temporal window)** — the transfer date must fall in TIER_1 (inside the
//!    application window, i.e. on/after `received_date` while undecided), TIER_2 (0–12
//!    months before), or TIER_3 (12–24 months before). >24 months or no anchor date → None.
//! 3. **gate-3 (connection corroboration)** — a ≥0.70 `auto_linked`/`confirmed`
//!    `identity_link` from the applicant's net (applicant + directors + PSCs) to a
//!    `property:*` node at the matched address (ENR-11 gate).
//!
//! If ANY gate fails, NO candidate is emitted. Passing candidates are written to
//! `land_flip_candidates` (a review queue) with `buyer_identity_confirmed: false` and a
//! MEDIUM severity ceiling — NEVER to `graph_events`/alerts. ≥3 sequential parcel
//! purchases at a postcode/street cluster downgrade the candidate to LOW with a stored
//! `suppression_reason`.
//!
//! # Security contract (T-23-FalseAccusation)
//!
//! "False accusations are the cardinal sin." The three-gate AND-requirement plus the
//! no-auto-naming / MEDIUM-ceiling invariant is the product's safety contract, enforced
//! in `build_candidate` (structural — `Severity` has no `High` variant) and exercised by
//! the named unit tests below with NO database.
//!
//! The pure gate/tier/suppression functions are DB-free so FLIP-02/03/04/05 are
//! unit-testable without SurrealDB, mirroring `offshore_shadow::process_link_rows`.

use std::collections::HashMap;

use chrono::{NaiveDate, Utc};
use serde_json::Value;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{info, warn};

#[cfg(test)]
use crate::adapters::land_registry_ppd::normalise_street;
use urban_integrity_core::persistence::land_flip_runs::{self, Gate1Survivor, LandFlipRunMarker};
use urban_integrity_ingestor::address::normalise_postcode;
use urban_integrity_ingestor::address::planning::parse_planning_address;

/// Temporal tier (gate-2). Defined relative to the planning `received_date`
/// (RESEARCH Target 4 / FLIP-02 tier definitions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Transfer on/after `received_date` — inside the (open, undecided) application window.
    Tier1,
    /// Transfer 0–12 months before `received_date`.
    Tier2,
    /// Transfer 12–24 months before `received_date`.
    Tier3,
}

impl Tier {
    /// Storage form written to `land_flip_candidates.tier`.
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Tier1 => "TIER_1",
            Tier::Tier2 => "TIER_2",
            Tier::Tier3 => "TIER_3",
        }
    }
}

/// Candidate severity. There is deliberately **NO `High` variant** — the MEDIUM ceiling
/// is a structural, compile-time-enforced invariant (FLIP-04). A land-flip candidate is a
/// review-queue signal about real entities and may never be auto-escalated to HIGH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Medium,
    Low,
}

impl Severity {
    /// Storage form written to `land_flip_candidates.severity`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Medium => "MEDIUM",
            Severity::Low => "LOW",
        }
    }
}

/// The PPD-transaction fields gate-1/build need (DB-free projection of a `ppd_transactions` row).
#[derive(Debug, Clone, PartialEq)]
pub struct PpdTxn {
    pub transaction_id: String,
    pub postcode_norm: String,
    pub paon_norm: String,
    pub street_norm: String,
    pub price_gbp: i64,
    pub date_of_transfer: NaiveDate,
}

/// A gate-3 corroborating identity link (the only thing that can let a candidate through).
#[derive(Debug, Clone, PartialEq)]
pub struct Corroboration {
    pub link_id: String,
    pub confidence: f64,
    pub entity_type: String,
}

/// A land-flip candidate destined for the `land_flip_candidates` review queue.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub tier: Tier,
    pub severity: Severity,
    pub status: String,
    pub buyer_identity_confirmed: bool,
    pub suppression_reason: Option<String>,
    pub date_source: String,

    pub ppd_transaction_id: String,
    pub ppd_postcode: String,
    pub ppd_paon: String,
    pub ppd_street: String,
    pub ppd_price_gbp: i64,
    pub ppd_date_of_transfer: NaiveDate,

    pub planning_ref: String,
    pub application_received_date: Option<NaiveDate>,
    pub temporal_offset_days: Option<i64>,

    pub corroborating_link_id: Option<String>,
    pub corroborating_link_confidence: Option<f64>,
    pub corroborating_entity_type: Option<String>,
}

/// gate-1: structural address match. ALL THREE of postcode/PAON/street (normalised) must
/// be equal. A postcode-only input (PAON or street differing/empty) returns `false` —
/// there is deliberately NO postcode-only fall-through (FLIP-03).
pub fn gate1_address_match(
    ppd_postcode_norm: &str,
    ppd_paon_norm: &str,
    ppd_street_norm: &str,
    app_postcode_norm: &str,
    app_paon_norm: &str,
    app_street_norm: &str,
) -> bool {
    // Empty normalised components can never satisfy an equality gate — guard explicitly so
    // two empty strings don't "match" and let a postcode-only row through.
    if ppd_paon_norm.is_empty()
        || ppd_street_norm.is_empty()
        || app_paon_norm.is_empty()
        || app_street_norm.is_empty()
    {
        return false;
    }
    ppd_postcode_norm == app_postcode_norm
        && ppd_paon_norm == app_paon_norm
        && ppd_street_norm == app_street_norm
}

/// How long after submission TIER_1's "application window" stays open, in days.
///
/// FLIP-02 defines TIER_1 as *"inside application window"*, but **no decision date exists
/// anywhere** — not in `SCHEMA_DEF`, not on `ContractData`, not in the scrapling sidecar's
/// output. So the window's close has to be proxied, and this constant is that proxy.
///
/// **Why 365.** UK statutory determination is 8 weeks (minor), 13 weeks (major) or 16 weeks
/// (EIA), but extensions of time are routine and appeals push far beyond that, so anything
/// near the statutory figure would miss genuine cases. 365 days is generous enough to cover
/// overruns while staying finite, and it makes the rule symmetric and explainable: a transfer
/// within twelve months *either side* of submission. TIER_2 is already 0–12 months before.
///
/// **This number is the operator-tunable part of gate-2** — a precision/recall trade-off, not
/// a fact. Widening it admits later transfers; narrowing it is the conservative direction, and
/// per D-16 a miss is preferable to a false accusation. What is NOT tunable is that the window
/// must close at all: an open-ended TIER_1 matched applications to transfers a median of 32
/// years later (planning open item 15).
const TIER1_WINDOW_DAYS: i64 = 365;

/// gate-2: temporal window classification (D-08 / RESEARCH Target 4).
///
/// Anchored on `received_date`. When `received_date` is `None` and `use_proxy` is `false`,
/// returns `None` — NEVER silently classifies everything TIER_3 (Pitfall 4). When
/// `use_proxy` is `true`, falls back to `proxy_date` as the anchor.
///
/// - `txn_date >= anchor`               → TIER_1 (inside the open application window)
/// - `0 < days_before <= 365`           → TIER_2 (0–12 months before submission)
/// - `365 < days_before <= 730`         → TIER_3 (12–24 months before submission)
/// - `days_before > 730`                → None (too old)
pub fn gate2_temporal(
    txn_date: NaiveDate,
    received_date: Option<NaiveDate>,
    use_proxy: bool,
    proxy_date: Option<NaiveDate>,
) -> Option<Tier> {
    let anchor = match received_date {
        Some(d) => d,
        None => {
            if use_proxy {
                proxy_date?
            } else {
                // Pitfall 4: no anchor date and proxy disabled → no candidate.
                return None;
            }
        }
    };

    if txn_date >= anchor {
        // The window has to close. Without this bound a transfer decades after submission
        // scores TIER_1 against an application decided long before — two unrelated events at
        // one address, not a flip. See TIER1_WINDOW_DAYS and planning open item 15.
        let days_after = (txn_date - anchor).num_days();
        return (days_after <= TIER1_WINDOW_DAYS).then_some(Tier::Tier1);
    }
    let days_before = (anchor - txn_date).num_days();
    if days_before <= 365 {
        Some(Tier::Tier2)
    } else if days_before <= 730 {
        Some(Tier::Tier3)
    } else {
        None
    }
}

/// Build a candidate once all three gates have passed. ALWAYS sets
/// `buyer_identity_confirmed = false` and `severity = Severity::Medium` (the MEDIUM ceiling
/// — FLIP-04). `temporal_offset_days` is signed (negative = pre-submission).
pub fn build_candidate(
    txn: &PpdTxn,
    planning_ref: &str,
    received_date: Option<NaiveDate>,
    tier: Tier,
    corr: &Corroboration,
    date_source: &str,
) -> Candidate {
    let temporal_offset_days = received_date.map(|r| (txn.date_of_transfer - r).num_days());
    Candidate {
        tier,
        severity: Severity::Medium,
        status: "pending_review".to_string(),
        buyer_identity_confirmed: false,
        suppression_reason: None,
        date_source: date_source.to_string(),

        ppd_transaction_id: txn.transaction_id.clone(),
        ppd_postcode: txn.postcode_norm.clone(),
        ppd_paon: txn.paon_norm.clone(),
        ppd_street: txn.street_norm.clone(),
        ppd_price_gbp: txn.price_gbp,
        ppd_date_of_transfer: txn.date_of_transfer,

        planning_ref: planning_ref.to_string(),
        application_received_date: received_date,
        temporal_offset_days,

        corroborating_link_id: Some(corr.link_id.clone()),
        corroborating_link_confidence: Some(corr.confidence),
        corroborating_entity_type: Some(corr.entity_type.clone()),
    }
}

/// Land-assembly suppression (FLIP-05). When `parcel_count >= 3` the candidate is downgraded
/// to LOW with a stored `suppression_reason`; otherwise it is returned unchanged.
///
/// NOTE (RESEARCH Assumption A3): `parcel_count` is grouped by `(postcode_norm, street_norm,
/// 24-month window)` — an ADDRESS-CLUSTER PROXY for "sequential purchases by the same entity",
/// because PPD carries NO buyer identity. This is the accepted proving-ground proxy per
/// A3/FLIP-05 intent; true entity-level grouping is DEFERRED to a future calibration phase.
pub fn maybe_suppress_land_assembly(mut candidate: Candidate, parcel_count: i64) -> Candidate {
    if parcel_count >= 3 {
        candidate.severity = Severity::Low;
        candidate.suppression_reason = Some(format!(
            "land_assembly_cluster: {} transactions at {}/{} in 24 months",
            parcel_count, candidate.ppd_postcode, candidate.ppd_street
        ));
    }
    candidate
}

// ── DB orchestration (run_detection) ────────────────────────────────────────

/// Stats returned by [`run_detection`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LandFlipStats {
    pub applications_scanned: usize,
    pub candidates_written: usize,
    /// Candidates that cleared every gate but whose `write_candidate` call errored.
    ///
    /// The write Result used to be dropped with `.is_ok()`. A systematic failure (schema drift
    /// on `land_flip_candidates`, a wedged instance) then left `candidate_apps > 0` against
    /// `candidates_written = 0` with nothing in the log saying why — in the one module whose
    /// whole stats structure exists so that a zero result is always attributable.
    pub candidate_writes_failed: usize,
    /// D-10: applications where a postcode-only PPD match exists but the full-triple
    /// gate-1 query (postcode+PAON+street) matched nothing. This is additive to — never a
    /// replacement of — the `continue` that still produces zero candidates for these; it
    /// only makes the signal VISIBLE rather than indistinguishable from "no data at all".
    pub postcode_only_matches: usize,
    /// Per-application rejection accounting (Open Item 16). Without these, `candidates_written
    /// = 0` reads identically whether gate-1 matched nothing, the application had no anchor
    /// date, every transfer fell outside the temporal window, or corroboration was absent —
    /// four different problems with four different fixes. Establishing that breakdown for the
    /// live corpus previously required re-deriving all three gates by hand outside the
    /// detector; that is what these make unnecessary.
    ///
    /// **Counted in the detector's own evaluation order, which is gate-1 → gate-3 → gate-2**
    /// (corroboration resolves once per application, before the per-transaction loop), NOT the
    /// order the gates are numbered in. An application rejected at gate-3 never reaches gate-2,
    /// so it is counted only in `gate3_no_corroboration`.
    ///
    /// These five plus `candidate_apps` partition `applications_scanned` exactly — see
    /// `stats_rejections_account_for_every_application`.
    pub gate1_no_ppd_match: usize,
    /// Applications where the gate-3 corroboration query itself errored — distinct from a
    /// clean "no link found", which is `gate3_no_corroboration`. A non-zero value here means
    /// results are incomplete, not that corroboration was absent.
    pub gate3_query_failed: usize,
    /// Gate-1 matched, but no ≥0.70 identity link connects the applicant to the property.
    pub gate3_no_corroboration: usize,
    /// Reached gate-2 with `received_date = None` and the ingestion-time proxy disabled, so
    /// gate-2 could never pass (Pitfall 4 — the deliberate no-anchor-date guard, not a bug).
    pub gate2_no_anchor_date: usize,
    /// Reached gate-2 with a usable anchor date, but every matched transfer fell outside the
    /// temporal window.
    pub gate2_outside_window: usize,
    /// Applications that produced at least one candidate. Distinct from `candidates_written`,
    /// which counts transactions and can exceed this when one application matches several.
    pub candidate_apps: usize,
}

/// Assembled context for one in-scope planning application.
struct AppContext {
    planning_ref: String,
    received_date: Option<NaiveDate>,
    applicant_id: String,
    app_postcode_norm: String,
    app_paon_norm: String,
    app_street_norm: String,
    location_id: String,
}

/// Assemble the per-application context list from the parsed rows of the `contract`,
/// `awarded_to`, `located_at` and `location` typed tables. Pure Rust logic over
/// already-parsed maps — no DB access — so this restructuring (D-04: gate-1's location
/// lookup keyed by `contract_id` rather than `applicant_id`) is unit-testable without a
/// live SurrealDB, mirroring `gate1_address_match`/`gate2_temporal`.
///
/// `applicant_id` is still resolved via `awarded_to` and carried on [`AppContext`] for
/// gate-3's corroboration lookup — only the `location_id` join changes: it now comes
/// directly from `located_at.get(&contract_id)`, not `located_at.get(&applicant_id)`.
fn assemble_app_contexts(
    contracts: &[(String, String, Option<NaiveDate>)],
    awarded_to: &HashMap<String, String>,
    located_at: &HashMap<String, String>,
    location_map: &HashMap<String, (Option<String>, String)>,
) -> Vec<AppContext> {
    let mut apps: Vec<AppContext> = Vec::new();
    for (contract_id, planning_ref, received_date) in contracts {
        let Some(applicant_id) = awarded_to.get(contract_id).cloned() else {
            continue;
        };
        // Post-D-04: keyed by contract_id directly, not applicant_id — the applicant
        // never needs to appear as a key in `located_at` for this lookup to succeed.
        let Some(location_id) = located_at.get(contract_id).cloned() else {
            continue;
        };
        let Some((postcode, address)) = location_map.get(&location_id) else {
            continue;
        };
        let (app_postcode_norm, app_paon_norm, app_street_norm) =
            normalise_app_address(postcode.as_deref(), address);
        // gate-1 requires all three components — skip applications we can't structurally match.
        if app_postcode_norm.is_empty() || app_paon_norm.is_empty() || app_street_norm.is_empty() {
            continue;
        }
        apps.push(AppContext {
            planning_ref: planning_ref.clone(),
            received_date: *received_date,
            applicant_id,
            app_postcode_norm,
            app_paon_norm,
            app_street_norm,
            location_id: location_id.clone(),
        });
    }
    apps
}

/// Convert a `NaiveDate` to a chrono `DateTime<Utc>` (midnight UTC).
///
/// Datetime values MUST be bound as chrono types — NOT serialised to RFC3339 strings via
/// `serde_json` — or SurrealDB 3.x rejects them on a `datetime` field ("Expected `datetime`
/// but found '...'"). See the staging-zero coercion bug.
fn naive_to_utc(d: NaiveDate) -> chrono::DateTime<Utc> {
    use chrono::TimeZone;
    Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
}

/// Parse a date-ish JSON value into a `NaiveDate`. Accepts "YYYY-MM-DD" and RFC3339.
fn parse_date_value(v: &Value) -> Option<NaiveDate> {
    let s = v.as_str()?;
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.date_naive())
}

/// Read a node-key string from a typed-table JSON value with SurrealDB RecordId coercion
/// (a related-record field may deserialise as a `{"tb":..,"id":..}` object or a quoted
/// record id rather than a plain string).
fn coerce_str(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    let repr = v.to_string();
    let repr = repr.trim_matches('"').to_string();
    if repr.is_empty() || repr == "null" {
        None
    } else {
        Some(repr)
    }
}

/// Derive the application's normalised address components from a Location address string,
/// using 29-07's structured planning-portal parser (`parse_planning_address`) rather than the
/// old flatten-and-regex approach, which could not distinguish a SAON from a PAON in principle
/// (D-12/D-13). Returns `(postcode_norm, paon_norm, street_norm)` — any empty component
/// disables gate-1 for that application (conservative; no postcode-only matching).
///
/// `postcode` is a separately-sourced signal (the `location` row's own `postcode` field,
/// populated by `extract_postcode` at ingest time in `mapper.rs`) and is preferred over the
/// parser's own re-extraction from the free-text `address` string when present — `Option::or`
/// only falls through to `parsed.postcode` when the caller passes `None`, so this function
/// never silently discards a signal it already receives. The parser's own `postcode` field is
/// already normalised internally, so routing it back through `normalise_postcode` here is a
/// no-op by construction (re-uppercasing/re-stripping an already-clean string yields the same
/// string) rather than a double-normalisation risk.
pub fn normalise_app_address(postcode: Option<&str>, address: &str) -> (String, String, String) {
    let parsed = parse_planning_address(address);
    let postcode_norm = postcode
        .or(parsed.postcode.as_deref())
        .and_then(normalise_postcode)
        .unwrap_or_default();
    let paon_norm = parsed.paon.unwrap_or_default();
    (postcode_norm, paon_norm, parsed.street)
}

/// Page size for the typed-table loads below. Small enough that one page's payload cannot
/// dominate an HTTP response, which is the condition under which a page comes back short.
const TABLE_PAGE_SIZE: usize = 20_000;

/// Safety cap so a pathological page sequence cannot loop forever. Hitting it is logged.
const TABLE_MAX_ROWS: usize = 2_000_000;

/// Every row of `table` matching `where_clause`, read to exhaustion.
///
/// `START` advances by rows **actually received** and the loop ends only on an empty page: a
/// short page means the response truncated, not that the table ran out. `ORDER BY id` keeps
/// page boundaries stable between requests, and `id` is therefore in the projection —
/// SurrealDB 3.x rejects an ORDER BY idiom that is not selected.
///
/// `fields`, `table` and `where_clause` are compile-time literals at every call site; no
/// caller-supplied value is interpolated.
async fn scan_table(
    db: &Surreal<Any>,
    fields: &str,
    table: &str,
    where_clause: &str,
) -> anyhow::Result<Vec<Value>> {
    scan_table_paged(db, fields, table, where_clause, TABLE_PAGE_SIZE).await
}

/// [`scan_table`] with an explicit page size, so a test can force multi-page behaviour without
/// seeding [`TABLE_PAGE_SIZE`] rows.
async fn scan_table_paged(
    db: &Surreal<Any>,
    fields: &str,
    table: &str,
    where_clause: &str,
    page_size: usize,
) -> anyhow::Result<Vec<Value>> {
    let mut out: Vec<Value> = Vec::new();
    let mut start = 0usize;
    loop {
        let sql = format!(
            "SELECT id, {fields} FROM {table} {where_clause} \
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<Value> = db.query(sql).await?.take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break;
        }
        out.extend(rows);
        start += returned;
        if start >= TABLE_MAX_ROWS {
            warn!(
                table,
                scanned = start,
                cap = TABLE_MAX_ROWS,
                "land_flip: table scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

/// Run the Pre-Permission Land Flip detector (FLIP-02/03/04/05).
///
/// Loads in-scope planning applications, runs the three mandatory gates against staged
/// PPD transactions, and writes passing candidates to `land_flip_candidates` (review
/// queue — NEVER `graph_events`). Returns counts; errors are non-fatal at the call site.
///
/// All SurrealQL uses equality / range / `IN` / `string::starts_with()` predicates and
/// bound parameters only — never `CONTAINS` (index-planner bug) and never string
/// interpolation of PPD/entity values (T-23-Tamper-Query).
pub async fn run_detection(db: &Surreal<Any>) -> anyhow::Result<LandFlipStats> {
    // The ingestion-time proxy for the temporal anchor is OFF by default (Pitfall 4):
    // applications without a real `received_date` are skipped rather than mis-tiered.
    const USE_INGESTION_TIME_AS_DATE_PROXY: bool = false;

    let mut stats = LandFlipStats::default();

    // ── Load planning applications + address linkage from the TYPED projection tables ──
    // (contract / location / awarded_to / located_at) — never graph_events. This mirrors
    // the PPD adapter (`SELECT postcode FROM location`) and gate-3 (`directs`/`controls`).

    // Every load below is paged. The ceilings these replace were sized to a past graph and had
    // already been overtaken: measured 2026-08-03 against `an_anglesey_aebaa785`, `located_at`
    // held **106,934** rows and `SELECT … LIMIT 100000` returned exactly 100,000 — 6,934
    // applicant→location relations dropped with no error and no signal. A detector that silently
    // reads part of its input reports "no candidates" identically to one that read all of it.
    //
    // Planning applications: the typed `contract` table, application_ref present.
    let contract_rows = scan_table(
        db,
        "entity_id, application_ref, application_received_date",
        "contract",
        "WHERE application_ref != NONE",
    )
    .await?;
    // awarded_to: applicant (in) → contract (out)
    let awarded_rows = scan_table(
        db,
        "in.entity_id AS applicant, out.entity_id AS contract",
        "awarded_to",
        "",
    )
    .await?;
    // located_at: contract (in) → location (out) — post-D-04, the mapper's new src.
    let located_rows = scan_table(
        db,
        "in.entity_id AS contract, out.entity_id AS location",
        "located_at",
        "",
    )
    .await?;
    // location: entity_id → (postcode, address name)
    let location_rows = scan_table(db, "entity_id, postcode, name", "location", "").await?;

    // Location nodes: location_id → (postcode, address)
    let mut location_map: HashMap<String, (Option<String>, String)> = HashMap::new();
    for row in &location_rows {
        if let Some(loc_id) = coerce_str(&row["entity_id"]) {
            let postcode = row["postcode"].as_str().map(|s| s.to_string());
            let address = row["name"].as_str().unwrap_or("").to_string();
            location_map.insert(loc_id, (postcode, address));
        }
    }

    // Planning contracts: (contract_id, planning_ref, received_date)
    let mut contracts: Vec<(String, String, Option<NaiveDate>)> = Vec::new();
    for row in &contract_rows {
        let (Some(contract_id), Some(planning_ref)) = (
            coerce_str(&row["entity_id"]),
            row["application_ref"].as_str().filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        let received = parse_date_value(&row["application_received_date"]);
        contracts.push((contract_id, planning_ref.to_string(), received));
    }

    // awarded_to map: contract → applicant ; located_at map: contract → location (D-04)
    let mut awarded_to: HashMap<String, String> = HashMap::new();
    for row in &awarded_rows {
        if let (Some(applicant), Some(contract)) =
            (coerce_str(&row["applicant"]), coerce_str(&row["contract"]))
        {
            awarded_to.insert(contract, applicant);
        }
    }
    let mut located_at: HashMap<String, String> = HashMap::new(); // contract_id -> location_id
    for row in &located_rows {
        if let (Some(contract), Some(location)) =
            (coerce_str(&row["contract"]), coerce_str(&row["location"]))
        {
            located_at.insert(contract, location);
        }
    }

    // ── Assemble per-application context ─────────────────────────────────────────────
    let apps = assemble_app_contexts(&contracts, &awarded_to, &located_at, &location_map);

    stats.applications_scanned = apps.len();

    let mut survivors: Vec<Gate1Survivor> = Vec::new();

    for app in &apps {
        // ── gate-1: structural address match against ppd_transactions (equality only) ──
        let ppd_rows: Vec<Value> = db
            .query(
                "SELECT transaction_id, postcode_norm, paon_norm, street_norm, price_gbp, \
                 date_of_transfer FROM ppd_transactions \
                 WHERE postcode_norm = $pc AND paon_norm = $paon AND street_norm = $street \
                 LIMIT 200",
            )
            .bind(("pc", app.app_postcode_norm.clone()))
            .bind(("paon", app.app_paon_norm.clone()))
            .bind(("street", app.app_street_norm.clone()))
            .await?
            .take(0)?;

        if ppd_rows.is_empty() {
            // D-10: make a postcode-matched-but-triple-missed application VISIBLE — it is
            // additive to the `continue` below (still zero candidates), not a replacement.
            if postcode_only_match_exists(db, &app.app_postcode_norm).await {
                stats.postcode_only_matches += 1;
            }
            stats.gate1_no_ppd_match += 1;
            continue;
        }

        // Every application that reaches this point cleared gate-1 (non-empty ppd_rows) —
        // collected during the loop, not re-derived afterward.
        survivors.push(Gate1Survivor {
            planning_ref: app.planning_ref.clone(),
            applicant_id: app.applicant_id.clone(),
            location_id: app.location_id.clone(),
        });

        // gate-3 corroboration is per-application (depends on the applicant net, not the
        // individual txn) — resolve once and reuse for every matched txn.
        let corroboration = match resolve_corroboration(db, &app.applicant_id).await {
            Ok(c) => c,
            Err(e) => {
                warn!(?e, planning_ref = %app.planning_ref, "land_flip: gate-3 query failed — skipping app");
                stats.gate3_query_failed += 1;
                continue;
            }
        };
        let Some(corr) = corroboration else {
            // gate-3 failed → no candidate for this application (the no-false-accusation guard).
            stats.gate3_no_corroboration += 1;
            continue;
        };

        // Gate-2 is evaluated per transaction below; track whether ANY of this application's
        // matched transfers cleared it, so the rejection can be attributed to a cause rather
        // than vanishing into an untracked `continue`.
        let mut app_cleared_gate2 = false;

        for row in &ppd_rows {
            let Some(txn) = parse_ppd_txn(row) else {
                continue;
            };
            // Belt-and-braces gate-1 re-check on the returned row (defends against any
            // query-side surprise — pure-function source of truth).
            if !gate1_address_match(
                &txn.postcode_norm,
                &txn.paon_norm,
                &txn.street_norm,
                &app.app_postcode_norm,
                &app.app_paon_norm,
                &app.app_street_norm,
            ) {
                continue;
            }

            // ── gate-2: temporal window ──
            let Some(tier) = gate2_temporal(
                txn.date_of_transfer,
                app.received_date,
                USE_INGESTION_TIME_AS_DATE_PROXY,
                None,
            ) else {
                continue;
            };
            let date_source = if app.received_date.is_some() {
                "portal_received_date"
            } else {
                "ingestion_proxy"
            };

            // ── all three gates passed → build + suppress + write ──
            let candidate = build_candidate(
                &txn,
                &app.planning_ref,
                app.received_date,
                tier,
                &corr,
                date_source,
            );
            let parcel_count = land_assembly_count(db, &txn.postcode_norm, &txn.street_norm).await;
            let candidate = maybe_suppress_land_assembly(candidate, parcel_count);

            app_cleared_gate2 = true;
            match write_candidate(db, &candidate).await {
                Ok(()) => stats.candidates_written += 1,
                Err(e) => {
                    stats.candidate_writes_failed += 1;
                    warn!(
                        error = %e,
                        planning_ref = %app.planning_ref,
                        "land_flip: candidate write FAILED"
                    );
                }
            }
        }

        // Attribute this application's outcome exactly once, so the five rejection counters
        // plus `candidate_apps` partition `applications_scanned`.
        if app_cleared_gate2 {
            stats.candidate_apps += 1;
        } else if app.received_date.is_none() && !USE_INGESTION_TIME_AS_DATE_PROXY {
            // Pitfall 4: no anchor date and the proxy is off, so gate-2 could never pass.
            stats.gate2_no_anchor_date += 1;
        } else {
            stats.gate2_outside_window += 1;
        }
    }

    if stats.candidates_written > 0 {
        info!(
            applications_scanned = stats.applications_scanned,
            candidates_written = stats.candidates_written,
            candidate_apps = stats.candidate_apps,
            postcode_only_matches = stats.postcode_only_matches,
            gate1_no_ppd_match = stats.gate1_no_ppd_match,
            gate3_no_corroboration = stats.gate3_no_corroboration,
            gate3_query_failed = stats.gate3_query_failed,
            gate2_no_anchor_date = stats.gate2_no_anchor_date,
            gate2_outside_window = stats.gate2_outside_window,
            candidate_writes_failed = stats.candidate_writes_failed,
            "land_flip: candidates written to land_flip_candidates review queue"
        );
    } else {
        // Deliberately INFO, not DEBUG. A zero result is the outcome that most needs
        // explaining, and emitting it at debug is how "0 candidates" went unexamined while
        // three separate structural defects hid behind it.
        info!(
            applications_scanned = stats.applications_scanned,
            postcode_only_matches = stats.postcode_only_matches,
            gate1_no_ppd_match = stats.gate1_no_ppd_match,
            gate3_no_corroboration = stats.gate3_no_corroboration,
            gate3_query_failed = stats.gate3_query_failed,
            gate2_no_anchor_date = stats.gate2_no_anchor_date,
            gate2_outside_window = stats.gate2_outside_window,
            // Non-zero here means the gates DID pass and the writes failed — a completely
            // different problem from "nothing qualified", and previously indistinguishable.
            candidate_writes_failed = stats.candidate_writes_failed,
            "land_flip: no candidates passed all three gates"
        );
    }

    // Both writes are unconditional (regardless of stats.candidates_written), WARN-only and
    // never discard their Result (mirrors persist_run_marker's local convention,
    // engine.rs:439-459). Ordering is load-bearing: write_gate1_survivors MUST execute and be
    // awaited BEFORE record_land_flip_run builds and writes the marker, so a crash between the
    // two writes leaves land_flip_runs:current un-stamped by THIS run rather than falsely
    // marked complete (D-02 extension).
    if let Err(e) = land_flip_runs::write_gate1_survivors(db, &survivors).await {
        warn!(error = %e, survivor_count = survivors.len(), "land_flip: failed to persist land_flip_gate1_survivors (non-fatal)");
    }
    let marker = LandFlipRunMarker {
        applications_scanned: stats.applications_scanned as i64,
        candidates_written: stats.candidates_written as i64,
        candidate_writes_failed: stats.candidate_writes_failed as i64,
        postcode_only_matches: stats.postcode_only_matches as i64,
        gate1_no_ppd_match: stats.gate1_no_ppd_match as i64,
        gate3_query_failed: stats.gate3_query_failed as i64,
        gate3_no_corroboration: stats.gate3_no_corroboration as i64,
        gate2_no_anchor_date: stats.gate2_no_anchor_date as i64,
        gate2_outside_window: stats.gate2_outside_window as i64,
        candidate_apps: stats.candidate_apps as i64,
        finished_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    if let Err(e) = land_flip_runs::record_land_flip_run(db, &marker).await {
        warn!(error = %e, "land_flip: failed to persist land_flip_runs marker (non-fatal)");
    }

    Ok(stats)
}

/// Parse a `ppd_transactions` SELECT row into a [`PpdTxn`].
fn parse_ppd_txn(row: &Value) -> Option<PpdTxn> {
    Some(PpdTxn {
        transaction_id: row["transaction_id"].as_str()?.to_string(),
        postcode_norm: row["postcode_norm"].as_str().unwrap_or("").to_string(),
        paon_norm: row["paon_norm"].as_str().unwrap_or("").to_string(),
        street_norm: row["street_norm"].as_str().unwrap_or("").to_string(),
        price_gbp: row["price_gbp"].as_i64().unwrap_or(0),
        date_of_transfer: parse_date_value(&row["date_of_transfer"])?,
    })
}

/// gate-3: resolve the applicant net (applicant + directors + PSCs) with the CORRECTED
/// traversal direction (the pattern notes — directors/PSCs are on the `in` side, filter
/// `out.entity_id = $applicant_id`) then find a ≥0.70 `auto_linked`/`confirmed`
/// `identity_link` from a `property:*` node into that net (ENR-11 gate). Returns the best
/// corroboration, or `None` if none qualifies.
async fn resolve_corroboration(
    db: &Surreal<Any>,
    applicant_id: &str,
) -> anyhow::Result<Option<Corroboration>> {
    let q = "\
        LET $directs = SELECT VALUE in.entity_id FROM directs \
            WHERE out.entity_id = $applicant_id \
              AND (meta.effective_to = NONE OR meta.effective_to > time::now()); \
        LET $controls = SELECT VALUE in.entity_id FROM controls \
            WHERE out.entity_id = $applicant_id \
              AND (meta.effective_to = NONE OR meta.effective_to > time::now()); \
        LET $net = array::union([$applicant_id], array::union($directs, $controls)); \
        SELECT id, source_node_id, target_node_id, confidence, status FROM identity_links \
            WHERE target_node_id IN $net \
              AND string::starts_with(source_node_id, 'property:') \
              AND confidence >= 0.70 \
              AND status IN ['auto_linked', 'confirmed'] \
            LIMIT 10;";
    let mut resp = db
        .query(q)
        .bind(("applicant_id", applicant_id.to_string()))
        .await?;
    // Statement index 3 is the final SELECT (0..2 are LET statements).
    let rows: Vec<Value> = resp.take(3)?;

    for row in &rows {
        // ENR-11 Rust-side belt-and-braces filter (mirrors offshore_shadow:53-65).
        let status = row["status"].as_str().unwrap_or("");
        if status != "auto_linked" && status != "confirmed" {
            continue;
        }
        let confidence = row["confidence"].as_f64().unwrap_or(0.0);
        if confidence < 0.70 {
            continue;
        }
        let link_id = match row["id"].as_str() {
            Some(s) => s.to_string(),
            None => row["id"].to_string().trim_matches('"').to_string(),
        };
        let entity_type = row["source_node_id"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "property".to_string());
        return Ok(Some(Corroboration {
            link_id,
            confidence,
            entity_type,
        }));
    }
    Ok(None)
}

/// D-10 diagnostic: true if `ppd_transactions` has at least one row at `postcode_norm`,
/// even though the full-triple gate-1 query (postcode+PAON+street) returned none for this
/// application. This makes a postcode-matched-but-address-missed application visible in
/// [`LandFlipStats::postcode_only_matches`] rather than indistinguishable from "no data at
/// all" — it does NOT relax gate-1's equality requirement (T-29-04a: bound parameter only,
/// no interpolation). Best-effort and diagnostic-only, mirroring `land_assembly_count`'s
/// shape: returns `false` on query error rather than propagating a hard failure.
async fn postcode_only_match_exists(db: &Surreal<Any>, postcode_norm: &str) -> bool {
    let result: surrealdb::Result<Vec<Value>> = db
        .query("SELECT count() FROM ppd_transactions WHERE postcode_norm = $pc GROUP ALL")
        .bind(("pc", postcode_norm.to_string()))
        .await
        .and_then(|mut r| r.take(0));
    result
        .ok()
        .and_then(|rows: Vec<Value>| rows.first().and_then(|r| r["count"].as_i64()))
        .map(|c| c > 0)
        .unwrap_or(false)
}

/// Count PPD transactions in the same postcode+street cluster within the last 24 months
/// (FLIP-05 land-assembly suppression proxy). Best-effort: returns 0 on query error.
async fn land_assembly_count(db: &Surreal<Any>, postcode_norm: &str, street_norm: &str) -> i64 {
    // Bind the cutoff as a chrono datetime so it compares against the `datetime` column.
    let cutoff = Utc::now() - chrono::Duration::days(730);
    let result: surrealdb::Result<Vec<Value>> = db
        .query(
            "SELECT count() FROM ppd_transactions \
             WHERE postcode_norm = $pc AND street_norm = $st AND date_of_transfer >= $cutoff \
             GROUP ALL",
        )
        .bind(("pc", postcode_norm.to_string()))
        .bind(("st", street_norm.to_string()))
        .bind(("cutoff", cutoff))
        .await
        .and_then(|mut r| r.take(0));
    result
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r["count"].as_i64()))
        .unwrap_or(0)
}

/// UPSERT a candidate into `land_flip_candidates` (review queue — NEVER `graph_events`).
/// De-duped by the natural key `(ppd_transaction_id, planning_ref)`.
async fn write_candidate(db: &Surreal<Any>, c: &Candidate) -> anyhow::Result<()> {
    let key = format!("{}__{}", c.ppd_transaction_id, c.planning_ref);
    // All-SET with bound primitives. Datetime fields (ppd_date_of_transfer,
    // application_received_date, created_at) are bound as chrono values so SurrealDB
    // serialises them as real `datetime` — NOT serde_json RFC3339 strings, which fail
    // coercion on a `datetime` column (see staging-zero bug). `.take(0)?` surfaces errors.
    // V5 / T-23-Tamper: parameterised binds only — never string-interpolate.
    db.query(
        "UPSERT type::record('land_flip_candidates', $key) SET \
           tier = $tier, severity = $severity, status = $status, \
           buyer_identity_confirmed = $buyer_identity_confirmed, \
           suppression_reason = $suppression_reason, date_source = $date_source, \
           ppd_transaction_id = $ppd_transaction_id, ppd_postcode = $ppd_postcode, \
           ppd_paon = $ppd_paon, ppd_street = $ppd_street, ppd_price_gbp = $ppd_price_gbp, \
           ppd_date_of_transfer = $ppd_date_of_transfer, planning_ref = $planning_ref, \
           application_received_date = $application_received_date, \
           temporal_offset_days = $temporal_offset_days, \
           corroborating_link_id = $corroborating_link_id, \
           corroborating_link_confidence = $corroborating_link_confidence, \
           corroborating_entity_type = $corroborating_entity_type, created_at = $created_at",
    )
    .bind(("key", key))
    .bind(("tier", c.tier.as_str()))
    .bind(("severity", c.severity.as_str()))
    .bind(("status", c.status.clone()))
    .bind(("buyer_identity_confirmed", c.buyer_identity_confirmed))
    .bind(("suppression_reason", c.suppression_reason.clone()))
    .bind(("date_source", c.date_source.clone()))
    .bind(("ppd_transaction_id", c.ppd_transaction_id.clone()))
    .bind(("ppd_postcode", c.ppd_postcode.clone()))
    .bind(("ppd_paon", c.ppd_paon.clone()))
    .bind(("ppd_street", c.ppd_street.clone()))
    .bind(("ppd_price_gbp", c.ppd_price_gbp))
    .bind(("ppd_date_of_transfer", naive_to_utc(c.ppd_date_of_transfer)))
    .bind(("planning_ref", c.planning_ref.clone()))
    .bind((
        "application_received_date",
        c.application_received_date.map(naive_to_utc),
    ))
    .bind(("temporal_offset_days", c.temporal_offset_days))
    .bind(("corroborating_link_id", c.corroborating_link_id.clone()))
    .bind((
        "corroborating_link_confidence",
        c.corroborating_link_confidence,
    ))
    .bind((
        "corroborating_entity_type",
        c.corroborating_entity_type.clone(),
    ))
    .bind(("created_at", Utc::now()))
    .await?
    .take::<Vec<serde_json::Value>>(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_txn() -> PpdTxn {
        PpdTxn {
            transaction_id: "TXN-1".to_string(),
            postcode_norm: "AB12CD".to_string(),
            paon_norm: "12".to_string(),
            street_norm: "high street".to_string(),
            price_gbp: 250_000,
            date_of_transfer: NaiveDate::from_ymd_opt(2024, 1, 15).unwrap(),
        }
    }

    fn sample_corr() -> Corroboration {
        Corroboration {
            link_id: "identity_links:abc".to_string(),
            confidence: 0.82,
            entity_type: "director".to_string(),
        }
    }

    // ── Pure-helper coverage (no DB) ────────────────────────────────────────────

    #[test]
    fn parse_date_value_accepts_ymd_and_rfc3339() {
        assert_eq!(
            parse_date_value(&serde_json::json!("2024-03-15")),
            Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap())
        );
        // RFC3339 with a time + zone → date part only.
        assert_eq!(
            parse_date_value(&serde_json::json!("2024-03-15T10:30:00Z")),
            Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap())
        );
    }

    #[test]
    fn parse_date_value_rejects_garbage_and_non_strings() {
        assert_eq!(parse_date_value(&serde_json::json!("not-a-date")), None);
        assert_eq!(parse_date_value(&serde_json::json!("15/03/2024")), None);
        // A JSON number is not a string → None (not coerced).
        assert_eq!(parse_date_value(&serde_json::json!(20240315)), None);
        assert_eq!(parse_date_value(&serde_json::json!(null)), None);
    }

    #[test]
    fn naive_to_utc_anchors_at_midnight() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let dt = naive_to_utc(d);
        assert_eq!(dt.date_naive(), d);
        assert_eq!(dt.format("%H:%M:%S").to_string(), "00:00:00");
    }

    #[test]
    fn coerce_str_handles_strings_empty_and_null() {
        assert_eq!(
            coerce_str(&serde_json::json!("director:john-smith")),
            Some("director:john-smith".to_string())
        );
        // Empty string → None (an empty key is unusable).
        assert_eq!(coerce_str(&serde_json::json!("")), None);
        // JSON null → None.
        assert_eq!(coerce_str(&serde_json::json!(null)), None);
        // Non-string scalar falls back to its quote-trimmed repr.
        assert_eq!(coerce_str(&serde_json::json!(42)), Some("42".to_string()));
    }

    #[test]
    fn parse_ppd_txn_parses_full_row() {
        let row = serde_json::json!({
            "transaction_id": "TXN-9",
            "postcode_norm": "AB12CD",
            "paon_norm": "42",
            "street_norm": "high street",
            "price_gbp": 250_000,
            "date_of_transfer": "2024-01-15",
        });
        let txn = parse_ppd_txn(&row).expect("full row must parse");
        assert_eq!(txn.transaction_id, "TXN-9");
        assert_eq!(txn.postcode_norm, "AB12CD");
        assert_eq!(txn.paon_norm, "42");
        assert_eq!(txn.street_norm, "high street");
        assert_eq!(txn.price_gbp, 250_000);
        assert_eq!(
            txn.date_of_transfer,
            NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()
        );
    }

    #[test]
    fn parse_ppd_txn_requires_id_and_date() {
        // Missing transaction_id → None.
        let no_id = serde_json::json!({ "date_of_transfer": "2024-01-15" });
        assert!(parse_ppd_txn(&no_id).is_none());
        // Missing/unparseable date → None.
        let no_date = serde_json::json!({ "transaction_id": "TXN-1" });
        assert!(parse_ppd_txn(&no_date).is_none());
    }

    #[test]
    fn parse_ppd_txn_defaults_optional_fields() {
        // Only the mandatory fields present → optional string fields default to "",
        // price defaults to 0.
        let row = serde_json::json!({
            "transaction_id": "TXN-2",
            "date_of_transfer": "2023-12-31",
        });
        let txn = parse_ppd_txn(&row).expect("mandatory-only row must parse");
        assert_eq!(txn.postcode_norm, "");
        assert_eq!(txn.paon_norm, "");
        assert_eq!(txn.street_norm, "");
        assert_eq!(txn.price_gbp, 0);
    }

    #[test]
    fn normalise_app_address_excludes_paon_from_street_post_parser() {
        // Replaces the stale D-12 pinning test, which asserted the OLD (wrong) behaviour —
        // street == "12 high street" with the PAON still embedded — as correct. Now wired
        // through 29-07's parse_planning_address, the PAON is EXCLUDED from the street.
        let (pc, paon, street) =
            normalise_app_address(Some("LL65 1NZ"), "86 Parc Tyddyn Bach, LL65 1NZ");
        assert_eq!(pc, "LL651NZ"); // upper-cased, de-spaced
        assert_eq!(paon, "86"); // leading numeric token, split off by the parser
        assert_eq!(street, "parc tyddyn bach"); // PAON no longer embedded in the street
        assert!(!street.contains("86"));
    }

    #[test]
    fn normalise_app_address_bare_number_segment_yields_paon_and_clean_street_not_fused_garbage() {
        // D-13: a comma-isolated leading number ("1, Deban, ...") must not produce the fused
        // "1debancrigyllroadrhosneigrll645qs"-shaped garbage the old flatten-and-regex approach
        // produced — the street must come out clean regardless of how the number is classified.
        //
        // [Gap-closure 2026-08-07, superseding this test's original name/assertions]: D-15's
        // original reading treated a comma-isolated leading number as a SAON-on-a-building,
        // never a PAON, asserting paon="" here. Measured against 9 confirmed genuine
        // (planning, PPD) pairs from the live an_anglesey_aebaa785 + PPD corpus (see
        // crates/ingestor/src/address/planning.rs's gap_closure_* fixtures, PPD's own
        // paon_norm/street_norm authority), that reading was wrong: it put an ordinary house
        // number in the one field gate-1 never reads, making a full (postcode, paon, street)
        // match structurally impossible. The bare number is now the PAON and the immediately
        // following segment is the street.
        let (pc, paon, street) = normalise_app_address(
            Some("LL64 5QS"),
            "1, Deban, Crigyll Road, Rhosneigr, LL64 5QS",
        );
        assert_eq!(pc, "LL645QS");
        assert_eq!(paon, "1"); // bare number segment -> PAON (gap-closure 2026-08-07)
        assert_eq!(street, "deban"); // segment immediately after the number, not "crigyll road"
    }

    #[test]
    fn normalise_street_and_planning_parser_agree_on_ppd_shaped_inputs() {
        // Carried issue from 29-07: `land_registry_ppd::normalise_street` (PPD side) and
        // `ingestor::address::planning`'s private `normalise_street_local` (planning side,
        // duplicated rather than imported because `crates/enricher` depends on
        // `crates/ingestor` — the reverse import is a circular crate dependency, per
        // the plan 29-07 summary) are two independent implementations of the same transform. Nothing
        // pins that they actually agree; if they ever drift, gate-1's per-string equality
        // silently stops matching. `normalise_street_local` is crate-private, so this test
        // exercises it indirectly through `normalise_app_address`'s public street output (which
        // now routes through `parse_planning_address`), over the same PPD-shaped street corpus
        // 29-01's `normalise_street_ppd_output_unchanged_by_planning_parser_work` tripwire pins.
        //
        // Read side-by-side, the two functions are in fact line-for-line identical (lowercase,
        // filter to alphanumeric + whitespace, collapse whitespace, single-space join) — this
        // test is what turns that observation into a standing regression guard instead of a
        // one-time comment.
        let streets = [
            "Crigyll Road",
            "Parc Tyddyn Bach",
            "Market Hill",
            "HIGH STREET",
            "O'Connell Street",
        ];
        for street in streets {
            let raw = format!("{street}, LL65 1AA");
            let (_, _, planning_street) = normalise_app_address(Some("LL65 1AA"), &raw);
            assert_eq!(
                normalise_street(street),
                planning_street,
                "PPD normalise_street and the planning-side parser disagree on {street:?}"
            );
        }
    }

    #[test]
    fn normalise_app_address_no_postcode_disables_gate1_component() {
        // No postcode → empty postcode_norm, which (per gate-1) disables postcode-only
        // matching. A named building (no leading number) → empty PAON.
        let (pc, paon, _street) = normalise_app_address(None, "Rose Cottage The Green");
        assert_eq!(pc, "");
        assert_eq!(paon, "");
    }

    #[test]
    fn normalise_app_address_rejects_too_short_postcode() {
        // "AB1" is too short for a valid UK postcode → normaliser returns None → "".
        let (pc, _paon, _street) = normalise_app_address(Some("AB1"), "5 Short Lane");
        assert_eq!(pc, "");
    }

    /// D-04 regression guard: `assemble_app_contexts`'s `location_id` join is keyed by
    /// `contract_id`, not `applicant_id` — the join must resolve even when the applicant
    /// never appears as a key in `located_at` at all (the post-fix mapper shape from
    /// 29-03, which sources `LocatedAt` from the contract, not the applicant).
    #[test]
    fn assemble_app_contexts_joins_located_at_via_contract_not_applicant() {
        let contracts = vec![(
            "contract:A".to_string(),
            "FUL/2024/001".to_string(),
            None::<NaiveDate>,
        )];
        let mut awarded_to: HashMap<String, String> = HashMap::new();
        awarded_to.insert("contract:A".to_string(), "entity:X".to_string());

        // located_at is keyed by contract_A — entity:X (the applicant) never appears as a
        // key here, which is exactly the post-D-04 mapper shape.
        let mut located_at: HashMap<String, String> = HashMap::new();
        located_at.insert("contract:A".to_string(), "location:1".to_string());

        let mut location_map: HashMap<String, (Option<String>, String)> = HashMap::new();
        location_map.insert(
            "location:1".to_string(),
            (Some("AB1 2CD".to_string()), "12 High Street".to_string()),
        );

        let apps = assemble_app_contexts(&contracts, &awarded_to, &located_at, &location_map);

        assert_eq!(
            apps.len(),
            1,
            "the join must succeed via contract_id alone, with entity:X never appearing as a located_at key"
        );
        assert_eq!(
            apps[0].applicant_id, "entity:X",
            "applicant_id is still resolved via awarded_to for gate-3"
        );
        assert_eq!(apps[0].app_postcode_norm, "AB12CD");
        assert_eq!(apps[0].app_paon_norm, "12");
        // 29-08: now routed through parse_planning_address, the PAON is excluded from the
        // street (was "12 high street" under the old flatten-and-regex normalise_app_address,
        // the same D-12 defect the dedicated normalise_app_address_* tests below pin correctly
        // — this assertion was collateral fallout from that bug, not this test's actual point,
        // which is the contract-keyed join proven above).
        assert_eq!(apps[0].app_street_norm, "high street");
    }

    /// The pre-fix shape (keyed by applicant) must NOT resolve under the new join: if
    /// `located_at` only has an applicant-keyed row (no contract-keyed row), the
    /// application is skipped rather than silently matched against the wrong location.
    #[test]
    fn assemble_app_contexts_does_not_fall_back_to_applicant_keyed_located_at() {
        let contracts = vec![(
            "contract:A".to_string(),
            "FUL/2024/001".to_string(),
            None::<NaiveDate>,
        )];
        let mut awarded_to: HashMap<String, String> = HashMap::new();
        awarded_to.insert("contract:A".to_string(), "entity:X".to_string());

        // Pre-fix (stale) shape: located_at keyed by the applicant, not the contract.
        let mut located_at: HashMap<String, String> = HashMap::new();
        located_at.insert("entity:X".to_string(), "location:1".to_string());

        let mut location_map: HashMap<String, (Option<String>, String)> = HashMap::new();
        location_map.insert(
            "location:1".to_string(),
            (Some("AB1 2CD".to_string()), "12 High Street".to_string()),
        );

        let apps = assemble_app_contexts(&contracts, &awarded_to, &located_at, &location_map);
        assert!(
            apps.is_empty(),
            "an applicant-keyed located_at row must not satisfy the post-D-04 contract-keyed join"
        );
    }

    /// FLIP-03 named negative test: a postcode match with PAON/street mismatch yields no match.
    #[test]
    fn gate1_postcode_only_produces_zero() {
        // postcode equal, PAON + street differ → must NOT match.
        assert!(
            !gate1_address_match("AB12CD", "12", "high street", "AB12CD", "99", "low road"),
            "postcode-only match must produce zero (FLIP-03)"
        );
        // postcode equal, application PAON/street empty (postcode-only input) → must NOT match.
        assert!(
            !gate1_address_match("AB12CD", "12", "high street", "AB12CD", "", ""),
            "empty PAON/street must never satisfy gate-1"
        );
    }

    /// gate-1 rejects when the postcode differs even though PAON + street are equal.
    #[test]
    fn gate1_reject() {
        assert!(
            !gate1_address_match("AB12CD", "12", "high street", "ZZ99ZZ", "12", "high street"),
            "differing postcode must reject"
        );
        // And accepts only when all three are equal.
        assert!(gate1_address_match(
            "AB12CD",
            "12",
            "high street",
            "AB12CD",
            "12",
            "high street"
        ));
    }

    /// D-09/LFA-04 regression guard: `gate1_address_match` takes no SAON parameter at all —
    /// SAON/flat information is never passed to this function. Two PPD rows that HMLR would
    /// distinguish only by SAON (e.g. "Flat 1" vs "Flat 2" at the same building) are therefore
    /// indistinguishable to gate-1 and BOTH correctly match on (postcode, paon, street) alone.
    /// A building-level match must never be blocked by a SAON difference this function never
    /// sees.
    #[test]
    fn gate1_saon_is_never_part_of_the_equality() {
        // Row A: PPD "Flat 1, 23 High Street" (SAON "Flat 1" is not a parameter here).
        // Row B: PPD "Flat 2, 23 High Street" (SAON "Flat 2" is not a parameter here).
        // Both are checked against the SAME planning application address — gate-1 sees only
        // (postcode, paon, street) for each side and cannot tell them apart.
        assert!(
            gate1_address_match("AB12CD", "23", "high street", "AB12CD", "23", "high street"),
            "SAON 'Flat 1' row must match on postcode+paon+street alone"
        );
        assert!(
            gate1_address_match("AB12CD", "23", "high street", "AB12CD", "23", "high street"),
            "SAON 'Flat 2' row must match on postcode+paon+street alone (gate-1 has no SAON param)"
        );
    }

    /// D-15/D-18 regression guard: a flat/SAON number expressed as `paon` on the planning
    /// side must NEVER manufacture a match against a different real PPD PAON at the same
    /// postcode/street. If a future change ever treated a small leading integer as "close
    /// enough" to a real PAON (fuzzy/near-match), this test fails first — exact-match-only
    /// (D-18) is the standing policy and this pins gate-1's exact-equality semantics
    /// specifically for the flat-vs-PAON shape (D-15 live-pair: planning `paon='1'` vs PPD
    /// `paon='23'` at the same postcode/street).
    #[test]
    fn gate1_flat_number_does_not_manufacture_a_match() {
        assert!(
            !gate1_address_match("AB12CD", "23", "high street", "AB12CD", "1", "high street"),
            "planning paon '1' (really a flat/SAON) must not exact-match PPD paon '23'"
        );
    }

    /// gate_all_pass: a fully-corroborated case (all three gates satisfied) BUILDS a candidate
    /// row (asserts a candidate is actually constructed, not just rejected) — FLIP-02.
    #[test]
    fn gate_all_pass() {
        let txn = sample_txn();
        let received = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();

        // gate-1
        assert!(gate1_address_match(
            &txn.postcode_norm,
            &txn.paon_norm,
            &txn.street_norm,
            "AB12CD",
            "12",
            "high street",
        ));
        // gate-2 (txn 2024-01-15 is ~4.5 months before 2024-06-01 → TIER_2)
        let tier = gate2_temporal(txn.date_of_transfer, Some(received), false, None)
            .expect("temporal gate must classify a txn 0–12mo before submission");
        assert_eq!(tier, Tier::Tier2);
        // gate-3 corroboration present → build a candidate
        let corr = sample_corr();
        let candidate = build_candidate(
            &txn,
            "FUL/2024/001",
            Some(received),
            tier,
            &corr,
            "portal_received_date",
        );

        assert_eq!(candidate.ppd_transaction_id, "TXN-1");
        assert_eq!(candidate.planning_ref, "FUL/2024/001");
        assert_eq!(candidate.tier, Tier::Tier2);
        assert_eq!(candidate.corroborating_link_confidence, Some(0.82));
        // signed temporal offset is negative (pre-submission)
        assert!(candidate.temporal_offset_days.unwrap() < 0);
    }

    /// candidate_invariants: every built candidate has `buyer_identity_confirmed == false`
    /// and severity is Medium or Low, NEVER High (the ceiling is structural).
    #[test]
    fn candidate_invariants() {
        let txn = sample_txn();
        let received = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        let corr = sample_corr();
        let candidate = build_candidate(
            &txn,
            "FUL/2024/001",
            Some(received),
            Tier::Tier1,
            &corr,
            "portal_received_date",
        );

        assert!(
            !candidate.buyer_identity_confirmed,
            "candidates must never be buyer-identity-confirmed by the detector (FLIP-04)"
        );
        assert!(
            matches!(candidate.severity, Severity::Medium | Severity::Low),
            "severity ceiling is MEDIUM — never High"
        );
        // The Severity enum has no High variant — this is a compile-time guarantee.
    }

    /// land_assembly_suppression: parcel_count>=3 sets severity LOW + a non-None
    /// suppression_reason; <3 leaves it MEDIUM (FLIP-05).
    #[test]
    fn land_assembly_suppression() {
        let txn = sample_txn();
        let received = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        let corr = sample_corr();
        let base = build_candidate(
            &txn,
            "FUL/2024/001",
            Some(received),
            Tier::Tier2,
            &corr,
            "portal_received_date",
        );

        // < 3 parcels → unchanged (MEDIUM, no suppression)
        let kept = maybe_suppress_land_assembly(base.clone(), 2);
        assert_eq!(kept.severity, Severity::Medium);
        assert!(kept.suppression_reason.is_none());

        // >= 3 parcels → LOW + suppression_reason set
        let suppressed = maybe_suppress_land_assembly(base, 4);
        assert_eq!(suppressed.severity, Severity::Low);
        assert!(suppressed
            .suppression_reason
            .as_deref()
            .is_some_and(|r| r.contains("land_assembly_cluster")));
    }

    /// Pitfall 4: gate-2 with received_date None and use_proxy false returns None —
    /// never silently classifies everything TIER_3.
    #[test]
    fn gate2_null_date_no_proxy_is_none() {
        let txn = sample_txn();
        assert_eq!(
            gate2_temporal(txn.date_of_transfer, None, false, None),
            None,
            "no anchor date + proxy off must yield no tier (Pitfall 4)"
        );
        // With proxy enabled and a proxy date, it classifies against the proxy anchor.
        let proxy = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
        assert!(
            gate2_temporal(txn.date_of_transfer, None, true, Some(proxy)).is_some(),
            "proxy anchor should classify when enabled"
        );
    }

    /// gate-2 tier boundaries: TIER_1 on/after submission, TIER_3 just inside 24 months,
    /// None just outside.
    #[test]
    fn gate2_tier_boundaries() {
        let received = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();
        // on/after submission → TIER_1
        assert_eq!(
            gate2_temporal(received, Some(received), false, None),
            Some(Tier::Tier1)
        );
        // 400 days before (≈13 months) → TIER_3
        let t3 = received - chrono::Duration::days(400);
        assert_eq!(
            gate2_temporal(t3, Some(received), false, None),
            Some(Tier::Tier3)
        );
        // 800 days before (>24 months) → None
        let too_old = received - chrono::Duration::days(800);
        assert_eq!(gate2_temporal(too_old, Some(received), false, None), None);
    }

    /// TIER_1's window must CLOSE. FLIP-02 defines TIER_1 as "inside application window",
    /// but the implementation accepted any `txn_date >= anchor` with no ceiling, so a
    /// transfer decades after submission scored TIER_1 on an application that was decided
    /// long before.
    ///
    /// Live effect on `an_anglesey_aebaa785` when the funnel first ran all three gates
    /// (2026-08-09): 146 of 199 gate-1 matches passed gate-2, **145 of them TIER_1**, at a
    /// median of 11,643 days — about 32 years — between application and transfer. They
    /// emitted nothing only because gate-3's corroboration rejected every one, leaving the
    /// margin against T-23-FalseAccusation one gate deep.
    ///
    /// There is no decision date anywhere in the schema, node types, or sidecar, so the
    /// window's close is proxied by `TIER1_WINDOW_DAYS`. See its doc for why 365.
    #[test]
    fn gate2_tier1_window_closes() {
        let received = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();

        // Submission day itself stays TIER_1 (unchanged behaviour).
        assert_eq!(
            gate2_temporal(received, Some(received), false, None),
            Some(Tier::Tier1)
        );
        // Last day inside the window — inclusive boundary.
        let edge = received + chrono::Duration::days(TIER1_WINDOW_DAYS);
        assert_eq!(
            gate2_temporal(edge, Some(received), false, None),
            Some(Tier::Tier1),
            "a transfer exactly TIER1_WINDOW_DAYS after submission is still inside the window"
        );
        // One day past it — the window is shut.
        let past = received + chrono::Duration::days(TIER1_WINDOW_DAYS + 1);
        assert_eq!(
            gate2_temporal(past, Some(received), false, None),
            None,
            "a transfer one day beyond the window must NOT be a candidate"
        );
        // The live shape: ~32 years after submission. Two unrelated events at one address.
        let decades_later = received + chrono::Duration::days(11_643);
        assert_eq!(
            gate2_temporal(decades_later, Some(received), false, None),
            None,
            "a transfer ~32 years after submission is not a land flip; it must not reach gate-3"
        );
    }

    /// The input loads must read past a page boundary (URBAN_DB_TESTS=1).
    ///
    /// The ceilings this replaces were sized to a past graph and had been overtaken:
    /// `located_at` held 106,934 rows against a `LIMIT 100000`, so 6,934 applicant→location
    /// relations vanished with no error. A detector that silently reads part of its input
    /// reports "no candidates" identically to one that read all of it, which is why this is
    /// asserted rather than trusted.
    #[tokio::test]
    async fn table_scan_reads_past_a_page_boundary() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        // Seed 5 rows, then read them 2 at a time: three pages, the last one short.
        for i in 0..5 {
            let _ = db
                .query("CREATE type::record('location', $k) SET entity_id = $eid, postcode = 'LL651AA', name = 'paging probe'")
                .bind(("k", format!("test-paging-{i}")))
                .bind(("eid", format!("test-paging-{i}")))
                .await
                .expect("seed location")
                .check()
                .expect("seed location statement error");
        }

        let rows = scan_table_paged(
            &db,
            "entity_id",
            "location",
            "WHERE name = 'paging probe'",
            2,
        )
        .await
        .expect("paged scan");
        assert_eq!(
            rows.len(),
            5,
            "paged scan lost rows across page boundaries: {rows:?}"
        );

        // The shape this replaces: one request with a ceiling. With the ceiling below the row
        // count it returns a truncated set and reports success — exactly what went unnoticed.
        let capped: Vec<Value> = db
            .query("SELECT entity_id FROM location WHERE name = 'paging probe' LIMIT 2")
            .await
            .expect("capped query")
            .take(0)
            .expect("capped rows");
        assert_eq!(
            capped.len(),
            2,
            "the pre-fix shape is expected to under-report — if this changed, revisit the fix"
        );

        for i in 0..5 {
            let _ = db
                .query("DELETE type::record('location', $k)")
                .bind(("k", format!("test-paging-{i}")))
                .await;
        }
    }

    /// D-10 (URBAN_DB_TESTS=1): `postcode_only_match_exists` returns `false` when no PPD
    /// row exists at a postcode at all, then `true` once a row is seeded at that postcode —
    /// proving the diagnostic that backs `LandFlipStats::postcode_only_matches` actually
    /// distinguishes "no data" from "postcode overlap but triple miss".
    #[tokio::test]
    async fn postcode_only_match_exists_distinguishes_no_data_from_postcode_overlap() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let txn_id = "TEST-FLIP-PCONLY-1";
        let postcode_norm = "ZZ99ZZ".to_string();

        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_id))
            .await;

        // No PPD row at this postcode at all → false.
        assert!(
            !postcode_only_match_exists(&db, &postcode_norm).await,
            "no PPD row at this postcode must report no overlap"
        );

        // Seed a row at the SAME postcode but a DIFFERENT paon/street — the point of
        // this diagnostic is that it counts a postcode overlap regardless of whether the
        // full triple would ever match.
        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 100000, date_of_transfer = $dot, \
               postcode_raw = 'ZZ9 9ZZ', postcode_norm = $pc, property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '99', paon_norm = '99', \
               street = 'OTHER ROAD', street_norm = 'other road', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_id))
        .bind(("tid", txn_id))
        .bind(("pc", postcode_norm.clone()))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap();

        assert!(
            postcode_only_match_exists(&db, &postcode_norm).await,
            "a PPD row at the same postcode (different paon/street) must report an overlap"
        );

        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_id))
            .await;
    }

    /// gate-3 negative (URBAN_DB_TESTS=1): a structurally-matched txn with a temporal hit
    /// but NO corroborating ≥0.70 auto_linked/confirmed `property:*` identity_link must
    /// write ZERO `land_flip_candidates` rows — the central no-false-accusation guard.
    #[tokio::test]
    async fn gate3_no_link_rejects() {
        // Gate: skip when URBAN_DB_TESTS is not set — never fail the unit run.
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }

        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let planning_ref = "TEST/FLIP/NOCORR";
        let txn_id = "TEST-FLIP-TXN-1";
        // Typed-table node keys (record-id key part) and full node-id strings.
        let entity_key = "test-flip-applicant";
        let contract_key = "test-flip-nocorr";
        let location_key = "test-flip-loc";
        let entity_nid = "entity:test-flip-applicant";
        let contract_nid = "contract:test-flip-nocorr";
        let location_nid = "location:test-flip-loc";

        // ── Clean any prior fixture rows ──
        let _ = db
            .query("DELETE land_flip_candidates WHERE planning_ref = $r")
            .bind(("r", planning_ref))
            .await;
        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_id))
            .await;
        // Relations first (delete by endpoints), then the typed node rows.
        let _ = db
            .query("DELETE awarded_to WHERE out.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE located_at WHERE in.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE type::record('entity', $k)")
            .bind(("k", entity_key))
            .await;
        let _ = db
            .query("DELETE type::record('contract', $k)")
            .bind(("k", contract_key))
            .await;
        let _ = db
            .query("DELETE type::record('location', $k)")
            .bind(("k", location_key))
            .await;

        // ── Seed a ppd_transactions row matching the application address ──
        // date_of_transfer bound as a chrono datetime (a serde_json string fails coercion
        // on the `datetime` column — the staging-zero bug this test would otherwise mask).
        // street_norm EXCLUDES the PAON ('high street', not '12 high street') — matching
        // both the PPD-side normaliser's real output (verified live against
        // an_anglesey_aebaa785: e.g. paon_norm="12", street_norm="well street") and, since
        // 29-08 Task 1, the app-side parser's output too. A stale '12 high street' fixture
        // here would make gate-1 fail to match BEFORE gate-3 ever runs, silently turning
        // this into a vacuous pass (rejected for the wrong reason) rather than a real
        // gate-3 negative.
        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 250000, date_of_transfer = $dot, \
               postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '12', paon_norm = '12', \
               street = 'HIGH STREET', street_norm = 'high street', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_id))
        .bind(("tid", txn_id))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap();

        // ── Seed the TYPED projection rows the detector reads ──
        db.query("UPSERT type::record('entity', $k) CONTENT $r")
            .bind(("k", entity_key))
            .bind((
                "r",
                serde_json::json!({"name":"Test Applicant Ltd","entity_id": entity_nid}),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed statement error");
        db.query("UPSERT type::record('contract', $k) CONTENT $r")
            .bind(("k", contract_key))
            .bind((
                "r",
                serde_json::json!({
                    "name":"test planning app","entity_id": contract_nid,
                    "application_ref": planning_ref, "application_received_date":"2024-06-01",
                    "ward_code":"E05000001",
                }),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed statement error");
        db.query("UPSERT type::record('location', $k) CONTENT $r")
            .bind(("k", location_key))
            .bind((
                "r",
                serde_json::json!({
                    "name":"12 HIGH STREET","entity_id": location_nid,"postcode":"AB1 2CD",
                }),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed statement error");
        // awarded_to: applicant → contract ; located_at: contract → location (D-04 — the
        // post-fix mapper shape, per 29-03; the pre-fix wiring related from the applicant).
        // RELATE needs record refs in the `->` positions, not `type::record()` calls — bind
        // the endpoints via LET first, then RELATE the variables. Both relation tables
        // require `meta.created_at` (datetime) and `data.details` (string) — SCHEMAFULL,
        // non-optional (`crates/schema/src/db.rs`). A RELATE that omits them fails
        // per-statement, but that failure is only visible via `.check()` — a bare
        // `.unwrap()` on the outer `surrealdb::Result` swallows it silently, which is
        // exactly what let this fixture pass vacuously (gate-1 never actually ran) before
        // this fix; `.check()` is now mandatory on every write in this test.
        db.query(
            "LET $a = type::record('entity', $e); \
             LET $c = type::record('contract', $ck); \
             RELATE $a->awarded_to->$c SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("e", entity_key))
        .bind(("ck", contract_key))
        .await
        .unwrap()
        .check()
        .expect("RELATE awarded_to failed");
        db.query(
            "LET $c = type::record('contract', $ck); \
             LET $l = type::record('location', $lk); \
             RELATE $c->located_at->$l SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("ck", contract_key))
        .bind(("lk", location_key))
        .await
        .unwrap()
        .check()
        .expect("RELATE located_at failed");

        // NOTE: NO identity_links seeded → gate-3 must fail.

        // ── Run the detector ──
        let stats = run_detection(&db).await.expect("run_detection failed");

        // Open Item 16: a zero must be ATTRIBUTABLE, not just zero. This fixture rejects at
        // gate-3 specifically, so that counter must move — asserting only `candidates_written
        // == 0` would pass identically if gate-1 had silently failed to match, which is
        // precisely the vacuous-pass this test was already fixed once for.
        assert!(
            stats.gate3_no_corroboration >= 1,
            "gate-3 negative must be attributed to gate3_no_corroboration, got {stats:?}"
        );

        // The six per-application outcome counters must partition applications_scanned
        // exactly — no application may fall through unattributed.
        let accounted = stats.gate1_no_ppd_match
            + stats.gate3_query_failed
            + stats.gate3_no_corroboration
            + stats.gate2_no_anchor_date
            + stats.gate2_outside_window
            + stats.candidate_apps;
        assert_eq!(
            accounted, stats.applications_scanned,
            "every scanned application must be accounted for exactly once, got {stats:?}"
        );

        // ── Assert ZERO candidates for this planning_ref ──
        let rows: Vec<Value> = db
            .query("SELECT count() FROM land_flip_candidates WHERE planning_ref = $r GROUP ALL")
            .bind(("r", planning_ref))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count = rows.first().and_then(|r| r["count"].as_i64()).unwrap_or(0);
        assert_eq!(
            count, 0,
            "gate-3 negative: an uncorroborated match must yield ZERO candidates (got {count})"
        );

        // ── Cleanup ──
        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_id))
            .await;
        let _ = db
            .query("DELETE awarded_to WHERE out.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE located_at WHERE in.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE type::record('entity', $k)")
            .bind(("k", entity_key))
            .await;
        let _ = db
            .query("DELETE type::record('contract', $k)")
            .bind(("k", contract_key))
            .await;
        let _ = db
            .query("DELETE type::record('location', $k)")
            .bind(("k", location_key))
            .await;
    }

    /// gate-3 positive (URBAN_DB_TESTS=1): a structurally-matched txn WITH a corroborating
    /// >=0.70 auto_linked `property:*` identity_link must actually produce a
    /// `land_flip_candidates` row. Added alongside the `string::starts_with()` fix — the
    /// only pre-existing gate-3 test (`gate3_no_link_rejects`) seeds NO identity_links, so
    /// it passed identically whether `resolve_corroboration`'s query executed correctly or
    /// errored out on invalid SurrealQL (the literal `STARTS WITH` operator is not valid
    /// SurrealQL 3.x syntax — verified live, `Parse error: Unexpected token`); that
    /// negative-only coverage is exactly how the bug went undetected. This test proves the
    /// query actually finds a real corroboration, not just that it fails safe when there is
    /// none.
    #[tokio::test]
    async fn gate3_positive_link_produces_candidate() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }

        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let planning_ref = "TEST/FLIP/CORR";
        let txn_id = "TEST-FLIP-TXN-CORR";
        let entity_key = "test-flip-corr-applicant";
        let contract_key = "test-flip-corr";
        let location_key = "test-flip-corr-loc";
        let entity_nid = "entity:test-flip-corr-applicant";
        let contract_nid = "contract:test-flip-corr";
        let location_nid = "location:test-flip-corr-loc";
        let property_nid = "property:test-flip-corr-prop";

        // ── Clean any prior fixture rows ──
        let _ = db
            .query("DELETE land_flip_candidates WHERE planning_ref = $r")
            .bind(("r", planning_ref))
            .await;
        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_id))
            .await;
        let _ = db
            .query("DELETE awarded_to WHERE out.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE located_at WHERE in.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE identity_links WHERE target_node_id = $e")
            .bind(("e", entity_nid))
            .await;
        let _ = db
            .query("DELETE type::record('entity', $k)")
            .bind(("k", entity_key))
            .await;
        let _ = db
            .query("DELETE type::record('contract', $k)")
            .bind(("k", contract_key))
            .await;
        let _ = db
            .query("DELETE type::record('location', $k)")
            .bind(("k", location_key))
            .await;

        // ── Seed a ppd_transactions row matching the application address ──
        // street_norm EXCLUDES the PAON — see gate3_no_link_rejects's comment on the same
        // fixture shape for why this must match both the real PPD normaliser's output and
        // the post-29-08-Task-1 app-side parser's output.
        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 250000, date_of_transfer = $dot, \
               postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '12', paon_norm = '12', \
               street = 'HIGH STREET', street_norm = 'high street', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_id))
        .bind(("tid", txn_id))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap();

        // ── Seed the TYPED projection rows the detector reads ──
        db.query("UPSERT type::record('entity', $k) CONTENT $r")
            .bind(("k", entity_key))
            .bind((
                "r",
                serde_json::json!({"name":"Test Corr Applicant Ltd","entity_id": entity_nid}),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed statement error");
        db.query("UPSERT type::record('contract', $k) CONTENT $r")
            .bind(("k", contract_key))
            .bind((
                "r",
                serde_json::json!({
                    "name":"test corr planning app","entity_id": contract_nid,
                    "application_ref": planning_ref, "application_received_date":"2024-06-01",
                    "ward_code":"E05000001",
                }),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed statement error");
        db.query("UPSERT type::record('location', $k) CONTENT $r")
            .bind(("k", location_key))
            .bind((
                "r",
                serde_json::json!({
                    "name":"12 HIGH STREET","entity_id": location_nid,"postcode":"AB1 2CD",
                }),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed statement error");
        db.query(
            "LET $a = type::record('entity', $e); \
             LET $c = type::record('contract', $ck); \
             RELATE $a->awarded_to->$c SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("e", entity_key))
        .bind(("ck", contract_key))
        .await
        .unwrap()
        .check()
        .expect("RELATE awarded_to failed");
        db.query(
            "LET $c = type::record('contract', $ck); \
             LET $l = type::record('location', $lk); \
             RELATE $c->located_at->$l SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("ck", contract_key))
        .bind(("lk", location_key))
        .await
        .unwrap()
        .check()
        .expect("RELATE located_at failed");

        // ── Seed the corroborating identity_link: property:* -> the applicant entity ──
        // All-SET + a bound chrono value for `resolved_at`, NOT `CONTENT` with a
        // serde_json-serialised datetime — SurrealDB 3.x does not coerce a JSON string into
        // a `datetime` field (the documented staging-zero coercion pitfall); `CONTENT`
        // serialises the whole object via serde_json first, so this bites even a `chrono`
        // value once it's nested inside a `json!({...})` call.
        db.query(
            "CREATE identity_links SET \
               source_node_id = $src, target_node_id = $tgt, resolution_type = $rt, \
               confidence = $conf, source = $src_name, resolved_at = $resolved_at, \
               evidence_json = $ev, status = $status",
        )
        .bind(("src", property_nid))
        .bind(("tgt", entity_nid.to_string()))
        .bind(("rt", "exact_title_number"))
        .bind(("conf", 0.90_f64))
        .bind(("src_name", "hmlr_ccod"))
        .bind((
            "resolved_at",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
        ))
        .bind(("ev", "{}"))
        .bind(("status", "auto_linked"))
        .await
        .unwrap()
        .check()
        .expect("CREATE identity_links failed");

        // ── Run the detector ──
        let stats = run_detection(&db).await.expect("run_detection failed");
        assert_eq!(
            stats.candidates_written, 1,
            "run_detection's own stats must also report exactly one candidate written"
        );

        // ── Assert exactly ONE candidate for this planning_ref, corroborated ──
        let rows: Vec<Value> = db
            .query(
                "SELECT count(), corroborating_link_id, corroborating_link_confidence \
                 FROM land_flip_candidates WHERE planning_ref = $r GROUP ALL",
            )
            .bind(("r", planning_ref))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count = rows.first().and_then(|r| r["count"].as_i64()).unwrap_or(0);
        assert_eq!(
            count, 1,
            "gate-3 positive: a real >=0.70 auto_linked property:* identity_link must \
             produce exactly one candidate (got {count}) — if this is 0, resolve_corroboration's \
             SurrealQL is broken again (e.g. an invalid `STARTS WITH` operator), not merely \
             'no corroboration found'"
        );

        // ── Cleanup ──
        let _ = db
            .query("DELETE land_flip_candidates WHERE planning_ref = $r")
            .bind(("r", planning_ref))
            .await;
        let _ = db
            .query("DELETE identity_links WHERE target_node_id = $e")
            .bind(("e", entity_nid))
            .await;
        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_id))
            .await;
        let _ = db
            .query("DELETE awarded_to WHERE out.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE located_at WHERE in.entity_id = $c")
            .bind(("c", contract_nid))
            .await;
        let _ = db
            .query("DELETE type::record('entity', $k)")
            .bind(("k", entity_key))
            .await;
        let _ = db
            .query("DELETE type::record('contract', $k)")
            .bind(("k", contract_key))
            .await;
        let _ = db
            .query("DELETE type::record('location', $k)")
            .bind(("k", location_key))
            .await;
    }

    /// Test-only helper: seed a full application fixture (entity/contract/location rows plus
    /// `awarded_to`/`located_at` relations) that `run_detection`'s typed-table scan picks up.
    /// Mirrors the seeding shape of `gate3_no_link_rejects`/`gate3_positive_link_produces_candidate`,
    /// factored out because Task 2's tests need to seed 1-2 applications each rather than one.
    async fn seed_app(
        db: &Surreal<Any>,
        entity_key: &str,
        contract_key: &str,
        location_key: &str,
        planning_ref: &str,
        address_name: &str,
        postcode: &str,
    ) {
        let entity_nid = format!("entity:{entity_key}");
        let contract_nid = format!("contract:{contract_key}");
        let location_nid = format!("location:{location_key}");

        db.query("UPSERT type::record('entity', $k) CONTENT $r")
            .bind(("k", entity_key.to_string()))
            .bind((
                "r",
                serde_json::json!({"name": format!("Test {entity_key}"), "entity_id": entity_nid}),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed entity");
        db.query("UPSERT type::record('contract', $k) CONTENT $r")
            .bind(("k", contract_key.to_string()))
            .bind((
                "r",
                serde_json::json!({
                    "name": format!("test {contract_key}"), "entity_id": contract_nid,
                    "application_ref": planning_ref, "application_received_date": "2024-06-01",
                    "ward_code": "E05000001",
                }),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed contract");
        db.query("UPSERT type::record('location', $k) CONTENT $r")
            .bind(("k", location_key.to_string()))
            .bind((
                "r",
                serde_json::json!({
                    "name": address_name, "entity_id": location_nid, "postcode": postcode,
                }),
            ))
            .await
            .unwrap()
            .check()
            .expect("seed location");

        db.query(
            "LET $a = type::record('entity', $e); \
             LET $c = type::record('contract', $ck); \
             RELATE $a->awarded_to->$c SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("e", entity_key.to_string()))
        .bind(("ck", contract_key.to_string()))
        .await
        .unwrap()
        .check()
        .expect("RELATE awarded_to failed");
        db.query(
            "LET $c = type::record('contract', $ck); \
             LET $l = type::record('location', $lk); \
             RELATE $c->located_at->$l SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("ck", contract_key.to_string()))
        .bind(("lk", location_key.to_string()))
        .await
        .unwrap()
        .check()
        .expect("RELATE located_at failed");
    }

    /// (D-01/D-03, URBAN_DB_TESTS=1) After a real `run_detection` call, `land_flip_runs:current`
    /// holds all 10 `LandFlipStats` fields plus `finished_at_ms`, matching the in-process
    /// `LandFlipStats` the SAME call returned — a positive field-by-field comparison, not "no
    /// error was logged" (Pitfall 5). Reuses the `TEST/FLIP/NOCORR` shape: one application, one
    /// matching PPD transaction, gate-1 passes, gate-3 fails (no identity_links seeded) so
    /// `gate3_no_corroboration == 1`, a known non-trivial stats shape.
    #[tokio::test]
    async fn run_detection_persists_land_flip_runs_marker_matching_in_process_stats() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let planning_ref = "TEST/FLIP/MARKER";
        let txn_id = "TEST-FLIP-MARKER-TXN-1";

        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 250000, date_of_transfer = $dot, \
               postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '12', paon_norm = '12', \
               street = 'HIGH STREET', street_norm = 'high street', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_id))
        .bind(("tid", txn_id))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap()
        .check()
        .expect("seed ppd_transactions");

        seed_app(
            &db,
            "test-flip-marker-applicant",
            "test-flip-marker-contract",
            "test-flip-marker-loc",
            planning_ref,
            "12 HIGH STREET",
            "AB1 2CD",
        )
        .await;

        // NOTE: NO identity_links seeded -> gate-3 must fail -> gate3_no_corroboration == 1.
        let stats = run_detection(&db).await.expect("run_detection failed");
        assert_eq!(
            stats.gate3_no_corroboration, 1,
            "fixture must reject at gate-3 for a known non-trivial stats shape, got {stats:?}"
        );

        let marker = land_flip_runs::get_land_flip_run_marker(&db)
            .await
            .expect("read marker")
            .expect("marker must be present after a real run_detection call");

        // Positive field-by-field comparison against the SAME call's in-process stats — all 10
        // LandFlipStats fields, not a subset.
        assert_eq!(
            marker.applications_scanned, stats.applications_scanned as i64,
            "applications_scanned mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.candidates_written, stats.candidates_written as i64,
            "candidates_written mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.candidate_writes_failed, stats.candidate_writes_failed as i64,
            "candidate_writes_failed mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.postcode_only_matches, stats.postcode_only_matches as i64,
            "postcode_only_matches mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.gate1_no_ppd_match, stats.gate1_no_ppd_match as i64,
            "gate1_no_ppd_match mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.gate3_query_failed, stats.gate3_query_failed as i64,
            "gate3_query_failed mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.gate3_no_corroboration, stats.gate3_no_corroboration as i64,
            "gate3_no_corroboration mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.gate2_no_anchor_date, stats.gate2_no_anchor_date as i64,
            "gate2_no_anchor_date mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.gate2_outside_window, stats.gate2_outside_window as i64,
            "gate2_outside_window mismatch: marker={marker:?} stats={stats:?}"
        );
        assert_eq!(
            marker.candidate_apps, stats.candidate_apps as i64,
            "candidate_apps mismatch: marker={marker:?} stats={stats:?}"
        );
        assert!(
            marker.finished_at_ms > 0,
            "finished_at_ms must be stamped, got {}",
            marker.finished_at_ms
        );
    }

    /// (D-06, URBAN_DB_TESTS=1) `land_flip_gate1_survivors` holds exactly one row per
    /// application that cleared gate-1 (`ppd_rows` non-empty), not per application that produced
    /// a written candidate. Two applications: one clears gate-1 (a matching PPD row exists), one
    /// does not (a different address, no matching PPD row) — neither has a corroborating
    /// identity_link, so NEITHER produces a `land_flip_candidates` row, yet exactly the
    /// gate-1-clearing one must appear as a survivor.
    #[tokio::test]
    async fn run_detection_persists_gate1_survivors_matching_ppd_matched_applications() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let matching_ref = "TEST/FLIP/SURV-MATCH";
        let nomatch_ref = "TEST/FLIP/SURV-NOMATCH";
        let txn_id = "TEST-FLIP-SURV-TXN-1";

        // Seed a PPD row at the MATCHING application's address only.
        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 250000, date_of_transfer = $dot, \
               postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '12', paon_norm = '12', \
               street = 'HIGH STREET', street_norm = 'high street', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_id))
        .bind(("tid", txn_id))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap()
        .check()
        .expect("seed ppd_transactions");

        // Application A: same address as the seeded PPD row -> clears gate-1.
        seed_app(
            &db,
            "test-flip-surv-a-applicant",
            "test-flip-surv-a-contract",
            "test-flip-surv-a-loc",
            matching_ref,
            "12 HIGH STREET",
            "AB1 2CD",
        )
        .await;
        // Application B: a different address with no matching PPD row -> fails gate-1.
        seed_app(
            &db,
            "test-flip-surv-b-applicant",
            "test-flip-surv-b-contract",
            "test-flip-surv-b-loc",
            nomatch_ref,
            "99 LOW ROAD",
            "ZZ9 9ZZ",
        )
        .await;

        let applicant_a_nid = "entity:test-flip-surv-a-applicant";
        let location_a_nid = "location:test-flip-surv-a-loc";

        let stats = run_detection(&db).await.expect("run_detection failed");
        assert_eq!(
            stats.applications_scanned, 2,
            "both applications must be scanned"
        );
        assert_eq!(
            stats.gate1_no_ppd_match, 1,
            "application B must fail gate-1, got {stats:?}"
        );

        let survivors = land_flip_runs::scan_land_flip_gate1_survivors_paged(&db, 20_000)
            .await
            .expect("scan survivors");

        assert!(
            survivors.iter().any(|s| s.planning_ref == matching_ref
                && s.applicant_id == applicant_a_nid
                && s.location_id == location_a_nid),
            "the gate-1-clearing application must be present as a survivor, got {survivors:?}"
        );
        assert!(
            !survivors.iter().any(|s| s.planning_ref == nomatch_ref),
            "the gate-1-failing application must NOT be present as a survivor, got {survivors:?}"
        );
    }

    /// A second `run_detection` call against a changed survivor set REPLACES the prior run's
    /// survivor rows rather than accumulating alongside them (DELETE-then-rebuild semantics,
    /// Task 1's design, mirroring `db_survivors_are_wiped_and_rebuilt_not_accumulated` at the
    /// persistence layer but exercised through the real detector this time).
    #[tokio::test]
    async fn run_detection_rerun_replaces_stale_survivors() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let ref_a = "TEST/FLIP/RERUN-A";
        let ref_b = "TEST/FLIP/RERUN-B";
        let txn_a = "TEST-FLIP-RERUN-TXN-A";
        let txn_b = "TEST-FLIP-RERUN-TXN-B";

        // Seed a PPD row matching application A's address only.
        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 250000, date_of_transfer = $dot, \
               postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '12', paon_norm = '12', \
               street = 'HIGH STREET', street_norm = 'high street', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_a))
        .bind(("tid", txn_a))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap()
        .check()
        .expect("seed ppd_transactions A");

        seed_app(
            &db,
            "test-flip-rerun-a-applicant",
            "test-flip-rerun-a-contract",
            "test-flip-rerun-a-loc",
            ref_a,
            "12 HIGH STREET",
            "AB1 2CD",
        )
        .await;

        // ── Run #1: A clears gate-1 ──
        let stats1 = run_detection(&db).await.expect("run_detection #1 failed");
        assert_eq!(
            stats1.gate1_no_ppd_match, 0,
            "A must clear gate-1 on run #1, got {stats1:?}"
        );

        let survivors1 = land_flip_runs::scan_land_flip_gate1_survivors_paged(&db, 20_000)
            .await
            .expect("scan #1");
        assert!(
            survivors1.iter().any(|s| s.planning_ref == ref_a),
            "A must be a survivor after run #1, got {survivors1:?}"
        );

        // ── Mutate: A's PPD match disappears; a NEW PPD row + application B appear ──
        let _ = db
            .query("DELETE ppd_transactions WHERE transaction_id = $t")
            .bind(("t", txn_a))
            .await;

        db.query(
            "UPSERT type::record('ppd_transactions', $key) SET \
               transaction_id = $tid, price_gbp = 300000, date_of_transfer = $dot, \
               postcode_raw = 'ZZ9 9ZZ', postcode_norm = 'ZZ99ZZ', property_type = 'D', \
               old_new = 'N', tenure = 'F', paon = '99', paon_norm = '99', \
               street = 'LOW ROAD', street_norm = 'low road', town = 'TOWNVILLE', \
               district = 'TEST', county = 'TEST', ppd_category = 'A', district_code = 'TEST'",
        )
        .bind(("key", txn_b))
        .bind(("tid", txn_b))
        .bind((
            "dot",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()),
        ))
        .await
        .unwrap()
        .check()
        .expect("seed ppd_transactions B");

        seed_app(
            &db,
            "test-flip-rerun-b-applicant",
            "test-flip-rerun-b-contract",
            "test-flip-rerun-b-loc",
            ref_b,
            "99 LOW ROAD",
            "ZZ9 9ZZ",
        )
        .await;

        // ── Run #2: A no longer clears gate-1 (its PPD row is gone); B now does ──
        let stats2 = run_detection(&db).await.expect("run_detection #2 failed");
        assert_eq!(
            stats2.applications_scanned, 2,
            "both A and B are in scope for run #2"
        );
        assert_eq!(
            stats2.gate1_no_ppd_match, 1,
            "A must now fail gate-1, got {stats2:?}"
        );

        let survivors2 = land_flip_runs::scan_land_flip_gate1_survivors_paged(&db, 20_000)
            .await
            .expect("scan #2");
        assert!(
            !survivors2.iter().any(|s| s.planning_ref == ref_a),
            "A must be REPLACED (DELETE-then-rebuild), not lingering from run #1, got {survivors2:?}"
        );
        assert!(
            survivors2.iter().any(|s| s.planning_ref == ref_b),
            "B must be the new survivor after run #2, got {survivors2:?}"
        );
    }

    /// (D-05, URBAN_DB_TESTS=1) The `crates/core` duplicate gate-3 predicate
    /// (`land_flip_capability::has_gate3_corroboration`) agrees with the private
    /// `resolve_corroboration` on IDENTICAL seeded rows in the SAME test — both called against
    /// the same fixture, not compared across two separate tests by inspection.
    #[tokio::test]
    async fn has_gate3_corroboration_agrees_with_resolve_corroboration_on_the_same_fixture() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        // ── CCOD-shaped: confidence 0.95, auto_linked -> both functions must agree TRUE ──
        let ccod_applicant = "entity:test-flip-dup-ccod-applicant";
        db.query(
            "CREATE identity_links SET \
               source_node_id = $src, target_node_id = $tgt, resolution_type = $rt, \
               confidence = $conf, source = $src_name, resolved_at = $resolved_at, \
               evidence_json = $ev, status = $status",
        )
        .bind(("src", "property:test-flip-dup-ccod-prop"))
        .bind(("tgt", ccod_applicant))
        .bind(("rt", "exact_title_number"))
        .bind(("conf", 0.95_f64))
        .bind(("src_name", "hmlr_ccod"))
        .bind((
            "resolved_at",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
        ))
        .bind(("ev", "{}"))
        .bind(("status", "auto_linked"))
        .await
        .unwrap()
        .check()
        .expect("seed CCOD-shaped identity_links");

        let resolve_ccod = resolve_corroboration(&db, ccod_applicant)
            .await
            .expect("resolve_corroboration query (CCOD)");
        let has_ccod =
            urban_integrity_core::persistence::land_flip_capability::has_gate3_corroboration(
                &db,
                ccod_applicant,
            )
            .await
            .expect("has_gate3_corroboration query (CCOD)");
        assert_eq!(
            resolve_ccod.is_some(),
            has_ccod,
            "resolve_corroboration and has_gate3_corroboration must agree on a CCOD-shaped link"
        );
        assert!(
            has_ccod,
            "a CCOD-shaped link (confidence>=0.70, auto_linked) must corroborate"
        );

        // ── OCOD-shaped: confidence 0.65, pending_review -> both functions must agree FALSE ──
        let ocod_applicant = "entity:test-flip-dup-ocod-applicant";
        db.query(
            "CREATE identity_links SET \
               source_node_id = $src, target_node_id = $tgt, resolution_type = $rt, \
               confidence = $conf, source = $src_name, resolved_at = $resolved_at, \
               evidence_json = $ev, status = $status",
        )
        .bind(("src", "property:test-flip-dup-ocod-prop"))
        .bind(("tgt", ocod_applicant))
        .bind(("rt", "exact_title_number"))
        .bind(("conf", 0.65_f64))
        .bind(("src_name", "hmlr_ocod"))
        .bind((
            "resolved_at",
            naive_to_utc(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
        ))
        .bind(("ev", "{}"))
        .bind(("status", "pending_review"))
        .await
        .unwrap()
        .check()
        .expect("seed OCOD-shaped identity_links");

        let resolve_ocod = resolve_corroboration(&db, ocod_applicant)
            .await
            .expect("resolve_corroboration query (OCOD)");
        let has_ocod =
            urban_integrity_core::persistence::land_flip_capability::has_gate3_corroboration(
                &db,
                ocod_applicant,
            )
            .await
            .expect("has_gate3_corroboration query (OCOD)");
        assert_eq!(
            resolve_ocod.is_some(),
            has_ocod,
            "resolve_corroboration and has_gate3_corroboration must agree on an OCOD-shaped link"
        );
        assert!(
            !has_ocod,
            "an OCOD-shaped link (confidence 0.65, pending_review) must not corroborate"
        );
    }
}
