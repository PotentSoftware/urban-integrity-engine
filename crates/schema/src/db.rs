pub const SCHEMA_DEF: &str = r#"
-- ── Node tables ──────────────────────────────────────────────────────────────

DEFINE TABLE IF NOT EXISTS entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name             ON entity TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id        ON entity TYPE string;
DEFINE FIELD IF NOT EXISTS company_number   ON entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS entity_status    ON entity TYPE option<string>;
-- The date the company entered its terminal state: date_of_cessation for Dissolved, the date
-- the status was set for InLiquidation. NULL for Active. `entity_status` alone is the flattened
-- `EntityStatus` discriminant and drops the date the enum carries; this column carries it.
-- RFC3339 string, not `datetime`: node rows are written via `upsert_node`'s `CONTENT $data`,
-- and SurrealDB 3.x will not coerce a string into a datetime field -- it fails the row silently.
DEFINE FIELD IF NOT EXISTS entity_status_date ON entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS incorporation_date ON entity TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_entity_company_number ON entity FIELDS company_number;
DEFINE INDEX IF NOT EXISTS idx_entity_entity_id      ON entity FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS officer SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name         ON officer TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id    ON officer TYPE string;
DEFINE FIELD IF NOT EXISTS officer_role ON officer TYPE option<string>;
DEFINE FIELD IF NOT EXISTS appointed_on ON officer TYPE option<string>;
DEFINE FIELD IF NOT EXISTS resigned_on  ON officer TYPE option<string>;
-- Company scope for the appointment. Officers are keyed `director:{slug}` (one human, one node),
-- so the id carries no company; anything scoping an officer to a company reads this instead.
DEFINE FIELD IF NOT EXISTS company_number ON officer TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_officer_entity_id ON officer FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS location SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name            ON location TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id       ON location TYPE string;
DEFINE FIELD IF NOT EXISTS postcode        ON location TYPE option<string>;
DEFINE FIELD IF NOT EXISTS address_line_1  ON location TYPE option<string>;
DEFINE FIELD IF NOT EXISTS locality        ON location TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_location_entity_id ON location FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS contract SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name             ON contract TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id        ON contract TYPE string;
DEFINE FIELD IF NOT EXISTS ocid             ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS value_gbp        ON contract TYPE option<float>;
DEFINE FIELD IF NOT EXISTS contract_status  ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS date_published   ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS ward_code        ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS tender_ref       ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS application_ref  ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS application_received_date ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS applicant_provenance ON contract TYPE option<string>;
DEFINE FIELD IF NOT EXISTS portal_detail_handle ON contract TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_contract_entity_id ON contract FIELDS entity_id;
DEFINE INDEX IF NOT EXISTS idx_contract_ward_code ON contract FIELDS ward_code;
DEFINE INDEX IF NOT EXISTS idx_contract_received_date ON contract FIELDS application_received_date;
DEFINE INDEX IF NOT EXISTS idx_contract_applicant_provenance ON contract FIELDS applicant_provenance;

DEFINE TABLE IF NOT EXISTS ward SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name       ON ward TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id  ON ward TYPE string;
DEFINE INDEX IF NOT EXISTS idx_ward_entity_id ON ward FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS political_actor SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name       ON political_actor TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id  ON political_actor TYPE string;
DEFINE FIELD IF NOT EXISTS party       ON political_actor TYPE option<string>;
DEFINE FIELD IF NOT EXISTS council_name ON political_actor TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_political_actor_entity_id ON political_actor FIELDS entity_id;

-- ── Relation tables ───────────────────────────────────────────────────────────

DEFINE TABLE IF NOT EXISTS directs TYPE RELATION FROM entity | officer TO entity | officer SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON directs TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON directs TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON directs TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.details        ON directs TYPE string;
DEFINE FIELD IF NOT EXISTS data.officer_role   ON directs TYPE option<string>;

DEFINE TABLE IF NOT EXISTS located_at TYPE RELATION FROM entity | contract TO location SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON located_at TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON located_at TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON located_at TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.details        ON located_at TYPE string;

DEFINE TABLE IF NOT EXISTS awarded_to TYPE RELATION FROM entity TO contract SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON awarded_to TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON awarded_to TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON awarded_to TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.details        ON awarded_to TYPE string;

DEFINE TABLE IF NOT EXISTS issued_by TYPE RELATION FROM entity TO contract SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON issued_by TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON issued_by TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON issued_by TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.details        ON issued_by TYPE string;

DEFINE TABLE IF NOT EXISTS represents TYPE RELATION FROM ward TO contract SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON represents TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON represents TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON represents TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.details        ON represents TYPE string;

-- ── OSINT node tables (Phase 16 / quick-260611-tuv) ─────────────────────────

DEFINE TABLE IF NOT EXISTS psc SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name                ON psc TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id           ON psc TYPE string;
DEFINE FIELD IF NOT EXISTS company_number      ON psc TYPE option<string>;
DEFINE FIELD IF NOT EXISTS natures_of_control  ON psc TYPE option<array>;
DEFINE INDEX IF NOT EXISTS idx_psc_entity_id   ON psc FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS charity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name              ON charity TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id         ON charity TYPE string;
DEFINE FIELD IF NOT EXISTS charity_number    ON charity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS registered_name   ON charity TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_charity_entity_id ON charity FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS property SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS entity_id      ON property TYPE string;
DEFINE FIELD IF NOT EXISTS title_number   ON property TYPE option<string>;
DEFINE FIELD IF NOT EXISTS tenure         ON property TYPE option<string>;
DEFINE FIELD IF NOT EXISTS price_paid     ON property TYPE option<float>;
DEFINE FIELD IF NOT EXISTS company_number ON property TYPE option<string>;
-- Canonical country of incorporation of an overseas corporate proprietor, set by the OCOD
-- adapter (Phase 22, PROP-05). NULL for CCOD (UK-company-owned) titles, which is what
-- distinguishes the two datasets on a projected row -- see `producer_of_property`.
DEFINE FIELD IF NOT EXISTS country_incorporated ON property TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_property_entity_id ON property FIELDS entity_id;

DEFINE TABLE IF NOT EXISTS offshore_entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name         ON offshore_entity TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id    ON offshore_entity TYPE string;
DEFINE FIELD IF NOT EXISTS jurisdiction ON offshore_entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS icij_node_id ON offshore_entity TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_offshore_entity_entity_id ON offshore_entity FIELDS entity_id;

-- A CDDA 1986 disqualification order. Its own node kind because the Companies House
-- disqualified-officers stream names no company: the order bars a person from directing any
-- company, so `disqualified_from` cannot point at an `entity` without asserting a
-- company-specific ban the source never states.
DEFINE TABLE IF NOT EXISTS disqualification SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS entity_id          ON disqualification TYPE string;
DEFINE FIELD IF NOT EXISTS case_number        ON disqualification TYPE option<string>;
-- RFC3339 strings, not `datetime`. Node rows are written with `CONTENT $data` from
-- serde_json (`upsert_node`), and SurrealDB 3.x will not coerce a string into a `datetime`
-- field — the same rule that governs `entity.incorporation_date`. The typed values live on
-- the `disqualified_from` edge, whose raw-SQL write path does apply the `<datetime>` cast.
DEFINE FIELD IF NOT EXISTS disqualified_from  ON disqualification TYPE option<string>;
DEFINE FIELD IF NOT EXISTS disqualified_until ON disqualification TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_disqualification_entity_id ON disqualification FIELDS entity_id;

-- ── OSINT relation tables (Phase 16 / quick-260611-tuv) ───────────────────────

DEFINE TABLE IF NOT EXISTS controls TYPE RELATION FROM psc TO entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON controls TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON controls TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON controls TYPE option<datetime>;

DEFINE TABLE IF NOT EXISTS trustee_of TYPE RELATION FROM entity TO charity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON trustee_of TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON trustee_of TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON trustee_of TYPE option<datetime>;

DEFINE TABLE IF NOT EXISTS owns_property TYPE RELATION FROM entity | offshore_entity TO property SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON owns_property TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON owns_property TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON owns_property TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.price_paid     ON owns_property TYPE option<float>;
DEFINE FIELD IF NOT EXISTS data.tenure         ON owns_property TYPE option<string>;

DEFINE TABLE IF NOT EXISTS offshore_link TYPE RELATION FROM entity TO offshore_entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON offshore_link TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON offshore_link TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON offshore_link TYPE option<datetime>;

DEFINE TABLE IF NOT EXISTS donates_to TYPE RELATION FROM entity TO political_actor SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at    ON donates_to TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON donates_to TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON donates_to TYPE option<datetime>;

DEFINE TABLE IF NOT EXISTS declares_interest TYPE RELATION FROM political_actor TO entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at     ON declares_interest TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from ON declares_interest TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to   ON declares_interest TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.council        ON declares_interest TYPE option<string>;
DEFINE FIELD IF NOT EXISTS data.category       ON declares_interest TYPE option<string>;

DEFINE TABLE IF NOT EXISTS disqualified_from TYPE RELATION FROM officer TO disqualification SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS meta.created_at        ON disqualified_from TYPE datetime;
DEFINE FIELD IF NOT EXISTS meta.effective_from     ON disqualified_from TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS meta.effective_to       ON disqualified_from TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.disqualified_from  ON disqualified_from TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.disqualified_until ON disqualified_from TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS data.case_number        ON disqualified_from TYPE option<string>;

-- ── WAL event log ─────────────────────────────────────────────────────────────
DEFINE TABLE IF NOT EXISTS graph_events SCHEMALESS;
DEFINE INDEX IF NOT EXISTS idx_graph_events_kind ON graph_events FIELDS kind;
DEFINE INDEX IF NOT EXISTS idx_graph_events_ts   ON graph_events FIELDS timestamp;

-- ── CH stream cursor ──────────────────────────────────────────────────────────
DEFINE TABLE IF NOT EXISTS ch_cursor SCHEMALESS;

-- ── Enricher stream cursors (Phase 17, CHA-03) ────────────────────────────────
-- Parameterised by stream name: enricher_cursors:ch_psc, enricher_cursors:ch_disqualified, etc.
-- Mirrors ch_cursor shape — schemaless, single `timepoint` field per record.
DEFINE TABLE IF NOT EXISTS enricher_cursors SCHEMALESS;

-- ── Backfill resume cursors (2026-07-28) ──────────────────────────────────────
-- One row per backfill job (backfill_cursors:ch), holding the record id of the last
-- candidate processed plus running totals. Lets a `backfill` run that dies mid-flight —
-- a SurrealDB session expiry, a lost network, a Ctrl-C — resume instead of re-issuing
-- every external lookup it already paid for. Schemaless, mirrors enricher_cursors.
DEFINE TABLE IF NOT EXISTS backfill_cursors SCHEMALESS;

-- ── Data anomalies (Phase 26 close-out) ──────────────────────────────────────
-- A durable destination for "the pipeline found something it could not safely resolve".
--
-- This project's characteristic failure is a wrong answer that logs nothing: a missing column
-- absorbed by `unwrap_or(0)`, a field dropped by a `json!` literal, a write whose Result was
-- discarded. Every one of those was found by accident, months later, because the only record
-- was a log line nobody kept. An anomaly row is queryable, survives the process, and shows up
-- in the evidence report, so the next one is found by looking rather than by luck.
--
-- Anomalies are NOT alerts. An alert says "this looks like corruption"; an anomaly says
-- "this input could not be trusted, and here is what was skipped".
--
-- The unique index makes re-runs idempotent: the same anomaly from the same source overwrites
-- rather than accumulating, which is the defect Open Item 6d found in alert persistence.
DEFINE TABLE IF NOT EXISTS data_anomalies SCHEMALESS;
DEFINE FIELD IF NOT EXISTS kind          ON data_anomalies TYPE string;
DEFINE FIELD IF NOT EXISTS severity      ON data_anomalies TYPE string;
DEFINE FIELD IF NOT EXISTS subject       ON data_anomalies TYPE string;
DEFINE FIELD IF NOT EXISTS detail        ON data_anomalies TYPE string;
DEFINE FIELD IF NOT EXISTS evidence_json ON data_anomalies TYPE string;
DEFINE FIELD IF NOT EXISTS source        ON data_anomalies TYPE string;
DEFINE FIELD IF NOT EXISTS detected_at   ON data_anomalies TYPE datetime;
DEFINE FIELD IF NOT EXISTS status        ON data_anomalies TYPE string;
DEFINE INDEX IF NOT EXISTS idx_anomalies_kind     ON data_anomalies FIELDS kind;
DEFINE INDEX IF NOT EXISTS idx_anomalies_status   ON data_anomalies FIELDS status;
DEFINE INDEX IF NOT EXISTS idx_anomalies_unique   ON data_anomalies FIELDS kind, subject, source UNIQUE;

-- ── Enrichment run markers (quick-260609-mhh) ─────────────────────────────────
-- Per-source enrichment run markers, parameterised by adapter name:
-- enrichment_runs:ch_psc, enrichment_runs:ch_disqualified, enrichment_runs:electoral_commission, etc.
-- Written best-effort after each adapter.run() Ok so the analyst probe can distinguish
-- "adapter ran, zero graph overlap" (marker present) from "adapter never ran" (no marker).
-- Schemaless, mirrors enricher_cursors — one record per adapter, counter fields per run.
DEFINE TABLE IF NOT EXISTS enrichment_runs SCHEMALESS;

-- ── Projection run marker (Open Item 30) ─────────────────────────────────────
-- Single record, projection_runs:current, rewritten by every `project` run.
--
-- `project` wipes the native tables and rebuilds them, so an interrupted run leaves a
-- partial projection that is indistinguishable from a complete one — no error, no log
-- line, just a smaller graph. That is exactly what happened on 2026-08-11: a run killed
-- 12.6% into the edge stream left five relation tables empty and four partial, and it
-- read as nine separate per-table bugs for four days.
--
-- `finished_at_ms` is written ONLY after the last batch flushes, so it is null for a run
-- that is still going, and stays null forever for one that died — including under SIGKILL,
-- which no in-process handler could catch. Any reader that cares whether the projected
-- tables are trustworthy checks that field first.
DEFINE TABLE IF NOT EXISTS projection_runs SCHEMALESS;

-- ── Detector run marker (Phase 32, VRD-06, operator decisions D1/D1a) ────────
-- Single record, detector_runs:current, rewritten every time the motif detector
-- engine actually runs inside GraphCoordinator::bake_snapshot.
--
-- `probe_precomputed_alerts` cannot honestly return Clear on zero MotifDetected
-- events alone: zero alerts is indistinguishable from "the motif detector never
-- ran at all". No marker of this kind existed anywhere before this table
-- (`enrichment_runs` covers OSINT adapters, `projection_runs` covers `project`,
-- `GraphCheckpoint` is a local-disk WAL-replay artifact — none of them observe
-- motif execution). See the phase 32 design notes, D1/D1a, for the full
-- rejection rationale.
--
-- Modelled on `projection_runs`'s "single/keyed record, success-stamp field
-- checked by readers, never row counts" idiom, not `enrichment_runs`'s
-- one-record-per-source idiom — D1 wants a single fact ("has the detector
-- engine run at all"), not per-adapter granularity.
--
-- `last_run_at` is a plain `i64` epoch-MILLISECOND value, never `TYPE datetime`
-- — SurrealDB 3.x will not coerce a string into a `datetime` field, and this
-- table is written via `UPSERT ... SET` with a bound `i64`, so there is no
-- coercion boundary to cross in the first place (same dodge `enrichment_runs`'s
-- writer already uses, `crates/enricher/src/engine.rs:414-459`).
--
-- Written from `crates/core` (`GraphCoordinator::bake_snapshot`), never from
-- `crates/detector` — D1a corrected D1's original framing after research found
-- `crates/detector` has zero SurrealDB dependency and its `run_tier_b` entry
-- point is synchronous and DB-free. `crates/detector` stays a pure, DB-free,
-- in-memory traversal engine.
--
-- Written ONLY on the detector's success path for a bake where a detector was
-- actually configured — a bake with `detector: None` must leave this table
-- completely untouched. A row with `motif_count: 0` would be indistinguishable
-- from "ran, found nothing", which is exactly the ambiguity this table exists
-- to remove.
--
-- Phase 46 (D-05) deliberately inverts this table from a single `:current` UPSERT
-- record to an append-only history table — one row per bake where a detector
-- actually ran, keyed by a fresh `run_id`, never overwritten. This is a conscious
-- departure from the `projection_runs`-style singleton idiom this comment block
-- previously chose; a future reader must not "fix" it back to a singleton citing
-- this file's own older reasoning.
DEFINE TABLE IF NOT EXISTS detector_runs SCHEMALESS;

-- ── Workbench report cache (Phase 43 plan 09 checkpoint fix, amending D-16) ──
-- Single record, workbench_report:latest, rewritten by every successful
-- `POST /api/analysis/run` in the analyst workbench.
--
-- D-16 originally read "the latest report per analysis is held in memory by
-- the workbench process ... No persistence" — every process restart forced a
-- fresh ~4.5-minute probe battery run before the Analysis/Home pages had
-- anything to show. The operator amended this at the plan-09 checkpoint
-- (2026-09-06): the latest report per analysis IS persisted, so a restart
-- seeds the in-memory cache instead of starting from "No probe run yet". See
-- the phase 43 design notes' dated amendment
-- under D-16 for the full record.
--
-- Holds the wire-shape `ReportView` (`view`, a JSON object — `Finding`/
-- `Verdict` never gain `Deserialize`, `ReportView` already has it per D-20),
-- the rendered Markdown dossier (`markdown`, `crates/probe::report::
-- render_evidence_report`'s output, rendered once at run time rather than
-- re-rendered from raw `Finding`s on every `GET /api/analysis/report.md`),
-- and `run_at` — modelled on `projection_runs`'s "single/keyed record,
-- success-stamp field checked by readers, never row counts" idiom. Written
-- and read only from `crates/workbench` (`crates/workbench/src/
-- report_persistence.rs`); a persist failure is logged via `tracing::warn!`
-- and never fails the run that produced it (graceful degradation, matching
-- the Ollama precedent docs/CONFIGURATION.md documents for `SummarizeAlert`).
--
-- `run_at` is bound as a chrono `DateTime<Utc>` via an all-`SET` UPSERT with
-- `.take(0)?`, never `CONTENT` with a string — SurrealDB 3.x will not coerce
-- a string into a `datetime` field (the datetime-coercion rule).
DEFINE TABLE IF NOT EXISTS workbench_report SCHEMALESS;

-- ── Identity resolution layer (Phase 16, ENR-06) ─────────────────────────────
-- evidence_json is TYPE string (never TYPE object) — 2000x perf regression
-- documented in the phase 16 research notes; storing as serialised JSON string sidesteps it.
DEFINE TABLE IF NOT EXISTS identity_links SCHEMALESS;
DEFINE FIELD IF NOT EXISTS source_node_id   ON identity_links TYPE string;
DEFINE FIELD IF NOT EXISTS target_node_id   ON identity_links TYPE string;
DEFINE FIELD IF NOT EXISTS resolution_type  ON identity_links TYPE string;
DEFINE FIELD IF NOT EXISTS confidence       ON identity_links TYPE float;
DEFINE FIELD IF NOT EXISTS source           ON identity_links TYPE string;
DEFINE FIELD IF NOT EXISTS resolved_at      ON identity_links TYPE datetime;
DEFINE FIELD IF NOT EXISTS evidence_json    ON identity_links TYPE string;
DEFINE FIELD IF NOT EXISTS status           ON identity_links TYPE string;
DEFINE INDEX IF NOT EXISTS idx_identity_links_source_node ON identity_links FIELDS source_node_id;
DEFINE INDEX IF NOT EXISTS idx_identity_links_target_node ON identity_links FIELDS target_node_id;
DEFINE INDEX IF NOT EXISTS idx_identity_links_status      ON identity_links FIELDS status;
DEFINE INDEX IF NOT EXISTS idx_identity_links_unique      ON identity_links FIELDS source_node_id, target_node_id, source UNIQUE;

-- ── ICIJ Offshore Leaks tables (Phase 19, FUZ-04) ────────────────────────────
-- ODbL share-alike isolation: icij_* prefix prevents mixed-table exports.
-- These tables are SCHEMALESS — ICIJ bulk CSVs have variable columns.

DEFINE TABLE IF NOT EXISTS icij_offshore_entity SCHEMALESS;
DEFINE FIELD IF NOT EXISTS node_id          ON icij_offshore_entity TYPE string;
DEFINE FIELD IF NOT EXISTS name             ON icij_offshore_entity TYPE string;
DEFINE FIELD IF NOT EXISTS jurisdiction     ON icij_offshore_entity TYPE option<string>;
DEFINE FIELD IF NOT EXISTS dataset          ON icij_offshore_entity TYPE string;
DEFINE INDEX IF NOT EXISTS idx_icij_entity_node_id  ON icij_offshore_entity FIELDS node_id UNIQUE;
DEFINE INDEX IF NOT EXISTS idx_icij_entity_name     ON icij_offshore_entity FIELDS name;

DEFINE TABLE IF NOT EXISTS icij_offshore_link SCHEMALESS;
DEFINE FIELD IF NOT EXISTS from_node_id     ON icij_offshore_link TYPE string;
DEFINE FIELD IF NOT EXISTS to_node_id       ON icij_offshore_link TYPE string;
DEFINE FIELD IF NOT EXISTS rel_type         ON icij_offshore_link TYPE string;
DEFINE FIELD IF NOT EXISTS dataset          ON icij_offshore_link TYPE string;
DEFINE INDEX IF NOT EXISTS idx_icij_link_from ON icij_offshore_link FIELDS from_node_id;
DEFINE INDEX IF NOT EXISTS idx_icij_link_to   ON icij_offshore_link FIELDS to_node_id;

-- ── OpenCorporates quota counter (Phase 20, POL-03) ──────────────────────────
-- One row per calendar month. year_month format: "2026-06".
-- Used by OpenCorporatesAdapter to enforce monthly_limit without external state.
-- Pitfall 4 note: free-tier monthly_limit defaults to 200 (not 500 — the roadmap is stale).
DEFINE TABLE IF NOT EXISTS oc_quota SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS year_month    ON oc_quota TYPE string;
DEFINE FIELD IF NOT EXISTS request_count ON oc_quota TYPE int DEFAULT 0;
DEFINE INDEX IF NOT EXISTS idx_oc_quota_month ON oc_quota FIELDS year_month UNIQUE;

-- ── PPD staging table (Phase 23, FLIP-01) ─────────────────────────────────────
-- OGL v3 open data — no ODbL isolation needed (cf. icij_* tables).
-- Raw PPD rows are NEVER projected as graph nodes (ICIJ staging pattern, FLIP-01).
-- Gate-1 join: postcode_norm + paon_norm + street_norm → match against Location nodes.
-- EXPLAIN ANALYZE prerequisite: run against the gate-1 join before detector ships.
DEFINE TABLE IF NOT EXISTS ppd_transactions SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS transaction_id     ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS price_gbp          ON ppd_transactions TYPE int;
DEFINE FIELD IF NOT EXISTS date_of_transfer   ON ppd_transactions TYPE datetime;
DEFINE FIELD IF NOT EXISTS postcode_raw       ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS postcode_norm      ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS property_type      ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS old_new            ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS tenure             ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS paon               ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS paon_norm          ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS saon               ON ppd_transactions TYPE option<string>;
DEFINE FIELD IF NOT EXISTS street             ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS street_norm        ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS locality           ON ppd_transactions TYPE option<string>;
DEFINE FIELD IF NOT EXISTS town               ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS district           ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS county             ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS ppd_category       ON ppd_transactions TYPE string;
DEFINE FIELD IF NOT EXISTS district_code      ON ppd_transactions TYPE string;
-- Idempotent natural key: transaction UUID (col 0) is globally unique (D-06).
DEFINE INDEX IF NOT EXISTS idx_ppd_txn_id        ON ppd_transactions FIELDS transaction_id UNIQUE;
-- Gate-1 join: postcode_norm + paon_norm composite is the primary equality path.
DEFINE INDEX IF NOT EXISTS idx_ppd_gate1         ON ppd_transactions FIELDS postcode_norm, paon_norm;
-- LPA-filtered query (district-scoped temporal scan for detector).
DEFINE INDEX IF NOT EXISTS idx_ppd_district_date ON ppd_transactions FIELDS district_code, date_of_transfer;
-- Land-assembly suppression: sequential parcel purchases (same street proximity).
DEFINE INDEX IF NOT EXISTS idx_ppd_parcel_date   ON ppd_transactions FIELDS postcode_norm, street_norm, date_of_transfer;

-- ── Land-flip analyst review queue (Phase 23, FLIP-04) ────────────────────────
-- NOT identity_links — dedicated table per D-01.
-- Status lifecycle: pending_review → confirmed | dismissed
-- buyer_identity_confirmed always starts false (FLIP-04 constraint).
-- Severity ceiling is MEDIUM — never HIGH (FLIP-04).
DEFINE TABLE IF NOT EXISTS land_flip_candidates SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS tier                         ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS severity                     ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS status                       ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS buyer_identity_confirmed     ON land_flip_candidates TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS suppression_reason           ON land_flip_candidates TYPE option<string>;
DEFINE FIELD IF NOT EXISTS date_source                  ON land_flip_candidates TYPE string;
-- Gate-1 evidence
DEFINE FIELD IF NOT EXISTS ppd_transaction_id           ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS ppd_postcode                 ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS ppd_paon                     ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS ppd_street                   ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS ppd_price_gbp                ON land_flip_candidates TYPE int;
DEFINE FIELD IF NOT EXISTS ppd_date_of_transfer         ON land_flip_candidates TYPE datetime;
-- Gate-2 evidence
DEFINE FIELD IF NOT EXISTS planning_ref                 ON land_flip_candidates TYPE string;
DEFINE FIELD IF NOT EXISTS application_received_date    ON land_flip_candidates TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS temporal_offset_days         ON land_flip_candidates TYPE option<int>;
-- Gate-3 evidence
DEFINE FIELD IF NOT EXISTS corroborating_link_id        ON land_flip_candidates TYPE option<string>;
DEFINE FIELD IF NOT EXISTS corroborating_link_confidence ON land_flip_candidates TYPE option<float>;
DEFINE FIELD IF NOT EXISTS corroborating_entity_type    ON land_flip_candidates TYPE option<string>;
-- Analyst workflow
DEFINE FIELD IF NOT EXISTS analyst_note                 ON land_flip_candidates TYPE option<string>;
DEFINE FIELD IF NOT EXISTS reviewed_at                  ON land_flip_candidates TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS created_at                   ON land_flip_candidates TYPE datetime;
-- Indexes
DEFINE INDEX IF NOT EXISTS idx_flip_status    ON land_flip_candidates FIELDS status;
DEFINE INDEX IF NOT EXISTS idx_flip_ppd_txn   ON land_flip_candidates FIELDS ppd_transaction_id;
DEFINE INDEX IF NOT EXISTS idx_flip_planning  ON land_flip_candidates FIELDS planning_ref;
-- De-dup: one candidate per (PPD transaction × planning application) pair (idempotent re-run).
DEFINE INDEX IF NOT EXISTS idx_flip_unique    ON land_flip_candidates FIELDS ppd_transaction_id, planning_ref UNIQUE;

-- ── Land-flip detector run marker (Phase 34, PRB-03, D-02) ────────────────────
-- Single record, land_flip_runs:current, rewritten every `run_detection` call.
--
-- crates/core cannot import crates/enricher (dependency direction: enricher ->
-- core, never the reverse), so the probe subcommand (crates/cli, PRB-03) needs
-- its own read path for land-flip run state that never existed until now --
-- the detector previously returned LandFlipStats in-process and discarded it
-- (D-01).
--
-- Holds all 10 LandFlipStats fields (crates/enricher/src/land_flip.rs:278-323)
-- verbatim, as plain i64 -- never TYPE datetime. SurrealDB 3.x will not
-- coerce a string into a datetime field, and this table is written via
-- UPSERT ... SET with bound i64 values, so there is no coercion boundary to
-- cross in the first place (same dodge detector_runs/enrichment_runs already
-- use).
--
-- finished_at_ms is set ONLY in the same UPSERT statement that sets every
-- other field, on the success path, so a reader can never observe a
-- partially-written row (D-02/D-04) -- the same "single/keyed record,
-- success-stamp field checked by readers, never row counts" idiom
-- projection_runs:current and detector_runs:current already use.
DEFINE TABLE IF NOT EXISTS land_flip_runs SCHEMALESS;

-- ── Land-flip gate-1 survivors (Phase 34, D-06) ────────────────────────────────
-- One row per gate-1 survivor as of the most recent land_flip_runs:current run:
-- (planning_ref, applicant_id, location_id). The gate-3 capability predicate
-- (D-05) needs each survivor's applicant_id to evaluate corroboration at probe
-- time -- a counts-only marker on land_flip_runs cannot supply that.
--
-- This table is DELETEd and rewritten WHOLE on every `run_detection` call
-- (Plan 34-03) -- a deliberate deviation from land_flip_candidates' own
-- accumulating-UPSERT-with-natural-key design (an analyst review queue). A
-- survivor that stops matching gate-1 between runs (e.g. after PPD data
-- changes) must not linger forever as a stale "capable" row, so there is
-- deliberately no UNIQUE dedup key here -- the index below exists for query
-- performance on a whole-table rebuild, not for upsert dedup.
DEFINE TABLE IF NOT EXISTS land_flip_gate1_survivors SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS planning_ref ON land_flip_gate1_survivors TYPE string;
DEFINE FIELD IF NOT EXISTS applicant_id ON land_flip_gate1_survivors TYPE string;
DEFINE FIELD IF NOT EXISTS location_id  ON land_flip_gate1_survivors TYPE string;
DEFINE INDEX IF NOT EXISTS idx_flip_survivors_app ON land_flip_gate1_survivors FIELDS planning_ref, applicant_id;

-- ── Saved investigations (Phase 45, PIV-05) ───────────────────────────────────
-- An analyst's work product: a named set of pinned graph members with free-text
-- notes. It lives in the ANALYSIS DATABASE, not in browser storage, because
-- phases 47-48 (dossier export, scoped export, subgraph extraction) consume the
-- same primitive server-side and `localStorage` would be invisible to them
-- (PIV-05, phase 45 design decision D-09).
--
-- `analysis_db` repeats the database name INSIDE the row on purpose. An analysis
-- id is the DATABASE, not the namespace (docs/CONFIGURATION.md) -- put it in the namespace
-- slot and SurrealDB returns zero rows instead of erroring. A row that travels
-- out of its own database (into a dossier, an export bundle, a bug report)
-- otherwise carries no statement of which analysis it describes, and the
-- connection that fetched it is not part of the row. The writer fills it from
-- SurrealQL's own `session::db()`, never from a caller-supplied string, so the
-- recorded analysis cannot disagree with the database the row actually landed
-- in -- a caller that passed the wrong name would otherwise stamp a row with an
-- analysis it is not in, and nothing downstream could tell.
--
-- TWO tables, not one row with a nested array of pins: a pin carries its own
-- note, its own `pinned_at` and its own `origin`, and 45-14 writes a whole
-- visible subgraph in one go. A nested array would put the analyst's notes
-- behind a read-modify-write of the entire set, where one slow editor's save
-- silently reverts every other pin's note.
--
-- DATETIMES ARE REAL `datetime` FIELDS HERE. They are written by binding a
-- chrono `DateTime<Utc>` through all-`SET` with `.take(0)?`
-- (crates/core/src/persistence/saved_investigations.rs, after
-- crates/ingestor/src/backfill/anomaly_sink.rs). SurrealDB 3.x will not coerce a
-- string into a `datetime` field -- `CONTENT` plus a bare `.await?` drops the row
-- silently, which cost this project 2.8m PPD rows reported as "2795151 written"
-- against 0 staged. The other tables in this file dodge the rule by storing int
-- milliseconds (`detector_runs`, `land_flip_runs`) or RFC3339 strings
-- (`entity.incorporation_date`); this one does not dodge it, because `updated_at`
-- is ordered on and a lexicographic sort over mixed-shape strings is a silent
-- wrong answer.
DEFINE TABLE IF NOT EXISTS saved_investigation SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name           ON saved_investigation TYPE string;
DEFINE FIELD IF NOT EXISTS analysis_db    ON saved_investigation TYPE string;
DEFINE FIELD IF NOT EXISTS notes          ON saved_investigation TYPE string DEFAULT '';
-- Bumped only when the stored shape changes meaning. A phase-47 reader can then
-- refuse an older row instead of reading an absent field as a default, which is
-- how "0 pins" and "pins this reader cannot see" become indistinguishable.
DEFINE FIELD IF NOT EXISTS schema_version ON saved_investigation TYPE int DEFAULT 1;
DEFINE FIELD IF NOT EXISTS created_at     ON saved_investigation TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at     ON saved_investigation TYPE datetime;
-- Names are NOT unique: two investigations may legitimately share one, and a
-- UNIQUE index here would turn a duplicate name into a failed save of analyst
-- work rather than a list with two similar rows.
DEFINE INDEX IF NOT EXISTS idx_saved_investigation_name     ON saved_investigation FIELDS name;
DEFINE INDEX IF NOT EXISTS idx_saved_investigation_analysis ON saved_investigation FIELDS analysis_db;

-- One row per pinned member. `kind` discriminates the two member shapes:
--   * `entity` -- `entity_id` set, the edge_* triple empty. PIV-05's pin button.
--   * `edge`   -- the edge_* triple set, `entity_id` empty. CNV-03/45-14 saves
--     the VISIBLE subgraph, and "exactly what was visible" cannot be recovered
--     from the node set alone: the canvas shows an edge-type-filtered subset of
--     the induced edges, so re-deriving them at export time would silently
--     restore edges the analyst had filtered out.
-- `member_key` is the deterministic natural key the record id is built from, so
-- pinning the same member twice UPSERTs one row rather than accumulating two
-- (PIV-05: "pinning the same entity twice does not create two entries"). The
-- UNIQUE index is the second witness: were the id derivation to stop being
-- deterministic, the duplicate fails loudly here instead of appearing as an
-- inflated entity count in the investigation list.
DEFINE TABLE IF NOT EXISTS saved_investigation_pin SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS investigation ON saved_investigation_pin TYPE record<saved_investigation>;
DEFINE FIELD IF NOT EXISTS kind          ON saved_investigation_pin TYPE string;
DEFINE FIELD IF NOT EXISTS member_key    ON saved_investigation_pin TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id     ON saved_investigation_pin TYPE option<string>;
DEFINE FIELD IF NOT EXISTS edge_from     ON saved_investigation_pin TYPE option<string>;
DEFINE FIELD IF NOT EXISTS edge_to       ON saved_investigation_pin TYPE option<string>;
DEFINE FIELD IF NOT EXISTS edge_relation ON saved_investigation_pin TYPE option<string>;
DEFINE FIELD IF NOT EXISTS note          ON saved_investigation_pin TYPE string DEFAULT '';
-- Which surface the member entered from -- `profile`, `canvas` or `subgraph`.
-- A dossier export has to say how a member got into the set: an entity the
-- analyst pinned by hand off a profile is a different claim from one that
-- arrived as part of a bulk subgraph save, and a set that cannot tell them apart
-- reads every member as deliberate.
DEFINE FIELD IF NOT EXISTS origin        ON saved_investigation_pin TYPE string;
-- When the member FIRST entered the set. Re-pinning preserves it (the writer's
-- `IF pinned_at != NONE THEN pinned_at ELSE $now END`), so an idempotent re-pin
-- cannot quietly restate when the analyst found something.
DEFINE FIELD IF NOT EXISTS pinned_at     ON saved_investigation_pin TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at    ON saved_investigation_pin TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_saved_pin_investigation ON saved_investigation_pin FIELDS investigation;
DEFINE INDEX IF NOT EXISTS idx_saved_pin_entity        ON saved_investigation_pin FIELDS entity_id;
DEFINE INDEX IF NOT EXISTS idx_saved_pin_member        ON saved_investigation_pin FIELDS investigation, member_key UNIQUE;
"#;

/// Schema for the `_catalog` database's `analyses` registry table.
///
/// SCHEMALESS on purpose: `created_at` is stored as an RFC3339 **string** to
/// avoid the SurrealDB 3.x string→datetime silent-coercion drop that bit the
/// PPD enricher (a SCHEMAFULL `datetime` field silently discards string input).
/// Fields (spec §5): db_name (record id + field), label, ward_codes,
/// wardset_hash, baseline_source, baseline_vintage, boundary_vintage,
/// schema_version, checkpoint_path, status, created_at, notes.
pub const CATALOG_SCHEMA: &str = r#"
DEFINE TABLE IF NOT EXISTS analyses SCHEMALESS;
DEFINE INDEX IF NOT EXISTS idx_analyses_db_name ON TABLE analyses COLUMNS db_name UNIQUE;
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify all 12 TYPE RELATION tables carry FROM/TO constraints (SCH-01, D-13, D-14).
    ///
    /// This is a pure string inspection test — no SurrealDB instance required.
    /// It catches DDL regressions at compile time.
    #[test]
    fn test_relation_constraints() {
        // All 12 tables must have FROM ... TO ... constraints. Raised from 11 on 2026-08-23
        // when `declares_interest` was added for POL-02 — see
        // the POL-02 directs-relate-rejected investigation notes.
        assert_eq!(
            SCHEMA_DEF.matches("TYPE RELATION FROM").count(),
            12,
            "Expected exactly 12 TYPE RELATION tables to carry FROM/TO constraints (SCH-01)"
        );

        // The ENFORCED keyword is prohibited (D-14: SurrealDB 3.x beta rejects it on
        // populated tables during ALTER TABLE — would break fresh-install DDL).
        assert!(
            !SCHEMA_DEF.contains("ENFORCED"),
            "ENFORCED keyword must not appear in SCHEMA_DEF (D-14)"
        );
    }

    /// Verify ppd_transactions SCHEMAFULL staging table DDL is present (FLIP-01, Plan 23-01).
    #[test]
    fn test_ppd_transactions_ddl() {
        assert!(
            SCHEMA_DEF.contains("DEFINE TABLE IF NOT EXISTS ppd_transactions SCHEMAFULL"),
            "ppd_transactions SCHEMAFULL table must be defined (FLIP-01)"
        );
        // Idempotent natural key — UNIQUE on transaction_id (D-06).
        assert!(
            SCHEMA_DEF.contains("idx_ppd_txn_id"),
            "idx_ppd_txn_id index must be defined on ppd_transactions"
        );
        assert!(
            SCHEMA_DEF.contains("transaction_id UNIQUE"),
            "idx_ppd_txn_id must be UNIQUE on transaction_id"
        );
        // Gate-1 composite index.
        assert!(
            SCHEMA_DEF.contains("idx_ppd_gate1"),
            "idx_ppd_gate1 must be defined on ppd_transactions"
        );
    }

    /// Verify land_flip_candidates SCHEMAFULL review-queue table DDL is present (FLIP-04).
    #[test]
    fn test_land_flip_candidates_ddl() {
        assert!(
            SCHEMA_DEF.contains("DEFINE TABLE IF NOT EXISTS land_flip_candidates SCHEMAFULL"),
            "land_flip_candidates SCHEMAFULL table must be defined (FLIP-04)"
        );
        // De-dup UNIQUE on (ppd_transaction_id, planning_ref).
        assert!(
            SCHEMA_DEF.contains("idx_flip_unique"),
            "idx_flip_unique must be defined on land_flip_candidates"
        );
        assert!(
            SCHEMA_DEF.contains("ppd_transaction_id, planning_ref UNIQUE"),
            "idx_flip_unique must be UNIQUE on (ppd_transaction_id, planning_ref)"
        );
    }

    /// Verify the D-02 applicant provenance DDL is present and never TYPE datetime (Phase 24,
    /// T-24-04) — SurrealDB 3.x silently drops rows on datetime coercion failure.
    #[test]
    fn test_contract_applicant_provenance_ddl() {
        assert!(
            SCHEMA_DEF.contains(
                "DEFINE FIELD IF NOT EXISTS applicant_provenance ON contract TYPE option<string>"
            ),
            "applicant_provenance field must be defined ON contract TYPE option<string>"
        );
        assert!(
            SCHEMA_DEF.contains(
                "DEFINE FIELD IF NOT EXISTS portal_detail_handle ON contract TYPE option<string>"
            ),
            "portal_detail_handle field must be defined ON contract TYPE option<string>"
        );
        for line in SCHEMA_DEF.lines() {
            if line.contains("applicant_provenance") || line.contains("portal_detail_handle") {
                assert!(
                    !line.contains("TYPE datetime"),
                    "applicant_provenance/portal_detail_handle must never be TYPE datetime: {line}"
                );
            }
        }
    }

    #[test]
    fn catalog_schema_defines_analyses_table() {
        assert!(
            CATALOG_SCHEMA.contains("DEFINE TABLE IF NOT EXISTS analyses SCHEMALESS"),
            "CATALOG_SCHEMA must define the SCHEMALESS analyses table"
        );
    }

    /// Tokenised DDL lines — whitespace-insensitive, comments excluded — so an assertion about
    /// the schema cannot be satisfied (or broken) by a comment or by column alignment.
    fn ddl_statements() -> Vec<Vec<&'static str>> {
        SCHEMA_DEF
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("DEFINE"))
            .map(|l| l.trim_end_matches(';').split_whitespace().collect())
            .collect()
    }

    /// The PIV-05 saved-investigation DDL: two tables, both SCHEMAFULL, both idempotent.
    ///
    /// PROVE THIS CAN FAIL: delete either `DEFINE TABLE` line from `SCHEMA_DEF`.
    #[test]
    fn saved_investigation_ddl_is_present_and_idempotent() {
        for table in ["saved_investigation", "saved_investigation_pin"] {
            assert!(
                SCHEMA_DEF.contains(&format!("DEFINE TABLE IF NOT EXISTS {table} SCHEMAFULL")),
                "{table} must be defined SCHEMAFULL with IF NOT EXISTS (PIV-05)"
            );
        }

        // Every statement this plan added must be re-runnable: `TestNs::connect` applies
        // `SCHEMA_DEF` unconditionally before every DB-gated test in the workspace, so one
        // non-idempotent line here fails several hundred tests that have nothing to do with
        // saved investigations.
        let mut checked = 0;
        for tokens in ddl_statements() {
            if !tokens.iter().any(|t| t.starts_with("saved_investigation")) {
                continue;
            }
            checked += 1;
            assert!(
                tokens.windows(3).any(|w| w == ["IF", "NOT", "EXISTS"]),
                "every saved-investigation DDL statement must be idempotent: {tokens:?}"
            );
        }
        // Non-vacuity: a typo in the table name would otherwise make the loop above check
        // nothing and pass. 2 tables + 6 investigation fields + 11 pin fields + 5 indexes.
        assert_eq!(
            checked, 24,
            "expected 24 saved-investigation DDL statements, found {checked} — update this \
             count deliberately when the shape changes"
        );
    }

    /// The four datetime fields are `TYPE datetime`, not strings.
    ///
    /// This is the assertion a future edit is most likely to "fix" the wrong way. Switching any
    /// of these to `TYPE string` would make every write succeed — including the round-trip test
    /// in `crates/core/tests/saved_investigations.rs` — while `updated_at` silently became a
    /// lexicographically-ordered text column, so the investigation list would sort by spelling
    /// and two ISO renderings of one instant would sort apart with nothing erroring.
    ///
    /// PROVE THIS CAN FAIL: change `created_at ON saved_investigation` to `TYPE string`.
    #[test]
    fn saved_investigation_timestamps_are_real_datetimes() {
        for (field, table) in [
            ("created_at", "saved_investigation"),
            ("updated_at", "saved_investigation"),
            ("pinned_at", "saved_investigation_pin"),
            ("updated_at", "saved_investigation_pin"),
        ] {
            let found = ddl_statements().into_iter().any(|t| {
                t.len() >= 10
                    && t[..5] == ["DEFINE", "FIELD", "IF", "NOT", "EXISTS"]
                    && t[5] == field
                    && t[6] == "ON"
                    && t[7] == table
                    && t[8] == "TYPE"
                    && t[9] == "datetime"
            });
            assert!(
                found,
                "{table}.{field} must be TYPE datetime — a string column would accept an \
                 RFC3339 blob and then sort it as text"
            );
        }
    }

    /// Pinning the same member twice must not create two entries (PIV-05).
    ///
    /// The writer's deterministic record id is the primary mechanism; this UNIQUE index is the
    /// second witness, so a regression in the id derivation fails loudly rather than showing up
    /// as an inflated entity count in the investigation list.
    ///
    /// PROVE THIS CAN FAIL: drop `UNIQUE` from `idx_saved_pin_member`.
    #[test]
    fn a_member_can_only_be_pinned_once_per_investigation() {
        let idx = ddl_statements()
            .into_iter()
            .find(|t| t.contains(&"idx_saved_pin_member"))
            .expect("idx_saved_pin_member must exist");
        assert_eq!(
            idx[idx.len() - 4..],
            ["FIELDS", "investigation,", "member_key", "UNIQUE"],
            "(investigation, member_key) must be UNIQUE — idempotent pinning is PIV-05's own \
             wording, and the index is what catches a non-deterministic record id"
        );
    }

    /// Neither new table is a RELATION, so the SCH-01 count of exactly 12 stays honest.
    #[test]
    fn saved_investigation_tables_are_not_relations() {
        for table in ["saved_investigation", "saved_investigation_pin"] {
            assert!(
                !SCHEMA_DEF.contains(&format!("{table} TYPE RELATION")),
                "{table} must not be a RELATION table"
            );
        }
    }

    /// New staging tables must NOT be TYPE RELATION — the relation-count test must stay at 11.
    #[test]
    fn test_ppd_tables_are_not_relations() {
        // Confirm neither new table is registered as a RELATION (which would break the
        // existing SCH-01 assertion of exactly 11 TYPE RELATION tables).
        assert!(
            !SCHEMA_DEF.contains("ppd_transactions TYPE RELATION"),
            "ppd_transactions must not be a RELATION table"
        );
        assert!(
            !SCHEMA_DEF.contains("land_flip_candidates TYPE RELATION"),
            "land_flip_candidates must not be a RELATION table"
        );
    }
}
