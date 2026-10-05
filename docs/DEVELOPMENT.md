<!-- generated-by: gsd-doc-writer -->
# Development Guide

<!-- GSD-MANAGED-DOC: development -->

Day-to-day development of the workspace: layout, toolchain, gates, and the per-subsystem rules
that are not derivable from reading the code. Sibling docs carry the rest — this guide
cross-references them rather than repeating them.

| Doc | Covers |
|-----|--------|
| [../README.md](../README.md) | What the engine does, the CLI surface, current status |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Crate dependency map, data pipeline, storage model, SurrealDB 3.x workarounds |
| [CONFIGURATION.md](CONFIGURATION.md) | Every `config.toml` block and `URBAN__*` environment variable |
| [API.md](API.md) | The gRPC / GraphQL / HTTP surface the `serve` command exposes |
| [TESTING.md](TESTING.md) | Test inventory, coverage gates, live-tier tests |
| [GETTING-STARTED.md](GETTING-STARTED.md) | First-run setup |
| [PROJECTION.md](PROJECTION.md) | The projection model in one page — why `graph_events` is the source of truth and the projected tables are a destructively rebuilt view |

---

## Standing Rules

Fourteen rules that have each already cost this project days. Each is repeated in context further
down; this is the list to read before touching persistence, adapter, or ingest code.

1. **Never discard a SurrealDB write `Result`.** `let _ = db.query(...)` compiles, logs success,
   and silently loses every row. The ICIJ adapter shipped exactly that and wrote **0 rows for its
   entire operational life** while reporting ~814k written. Always `.await?` **and** consume the
   response with `.take(...)` / `.check()`, so per-row errors surface — and then **assert the write
   returned a row**, because SurrealDB reports `status: OK` with an empty result for an UPSERT that
   matched nothing. The canonical fail-loud pattern is `record_anomaly` in
   [`crates/ingestor/src/backfill/anomaly_sink.rs`](../crates/ingestor/src/backfill/anomaly_sink.rs):
   all-`SET`, `.take(0)?`, then `bail!` on an empty `Vec`. Copy that shape.
2. **SurrealDB 3.x will not coerce a string into a `datetime` field.** Bind a chrono
   `DateTime<Utc>` (or use SurrealQL's own `time::now()`), write with all-`SET` rather than
   `CONTENT $row`, and consume the response with `.take(0)?`. A `serde_json` `CONTENT` write with a
   bare `.await?` fails **silently**: the query returns `Ok` while staging zero rows. That cost
   **2,795,151 PPD rows** — the run reported them written against a table that stayed empty, and
   every unit test was green. Do not look in `crates/core/src/persistence/` for
   "existing date workarounds": that layer stores `timestamp` as a `u64`
   millisecond integer and never hits the coercion rule at all.
3. **Validate rows against `SCHEMA_DEF` before trusting a write.** SurrealDB fails a
   shape-mismatched row **silently** rather than erroring — a missing required field stages nothing
   while the query returns `Ok`. Round-trip at least one row against the real table definition in a
   test.
4. **Never pair an indexed equality with an unindexed predicate in the same read.** On
   `graph_events` that shape is `kind = '…'` beside a `CONTAINS` on `payload.*`: the planner takes
   the index path and returns a truncated row set while `count()` on the identical predicate stays
   correct. Adding indexes is *not* the fix — index availability is what triggers the misplan. The
   source-level guard now watches **nine** tables (`graph_events`, `identity_links`,
   `enrichment_runs`, `oc_quota`, `land_flip_candidates`, `ppd_transactions`, `land_flip_runs`,
   `land_flip_gate1_survivors`, `analyses`) with a
   per-table `LIMIT` threshold, and a third shape: an **ordered read with a `LIMIT` and no
   `START`**, which biases the sample toward one end of the ordering at any magnitude. See [Query
   Shapes That Lose Rows Silently](#query-shapes-that-lose-rows-silently).
5. **Never full-reingest PPD to test idempotency.** Idempotency there is *structural* — an UPSERT
   keyed on the PPD `transaction_id`. Confirm it with an unchanged `count()` or a one-year re-run.
   A redundant full national re-pass thrashes SurrealDB's background tasks for hours.
6. **Never run `surreal start` by hand.** Use `scripts/surreal-start.sh`, which pins the RocksDB
   backend and the cgroup memory cap. A bare `surreal start` escapes both. If the engine is up on
   `/health` but queries hang, stop it first — `systemctl --user stop urban-surreal`, then
   `scripts/surreal-start.sh`; the launcher will not replace a healthy-but-wedged instance.
   **And never run `scripts/surreal-start.sh` from a git worktree.** It derives the service's
   `WorkingDirectory` from its own location and the store path is relative, so a worktree launch
   silently creates and serves a *fresh, empty* store — `/health` returns 200, the schema applies,
   every DB-gated test passes, and the namespace holding the real corpus is not there at all. Only
   the run *duration* dissents. See [Never launch from a worktree](#never-launch-from-a-worktree).
7. **`project` is destructive-then-rebuild, not incremental.** It wipes each half of the native
   schema — node tables, then relation tables — and re-derives it from the WAL. The wipe now
   happens *after* the corresponding stream has loaded, so a failed load no longer costs the
   existing projection, but a run killed mid-write still leaves tables indistinguishable from a
   finished small graph — no error, no log line. Only `projection_runs:current.finished_at_ms` tells the two
   apart. A full projection over the live corpus takes **well over an hour** and starves concurrent
   reads; do not start one you cannot let finish. See
   [CQRS Projector § Run marker](#run-marker-partial-vs-complete).
8. **Strip SurrealDB's display quoting before addressing a record by id.** SurrealDB renders any
   record id whose key is not a bare identifier in quoted form — live `graph_events` ids come back
   as ``graph_events:`NodeAdded_GB-OCOD_1811435-fc013582_GB-OCOD_1811435-fc013582` ``. The
   backticks are *display syntax*, not part of the key, so handing them to
   `RecordId::new(table, key)` addresses a key whose literal characters include the quote marks —
   which matches no row. On 2026-08-22 the first live OCOD owner purge sent 10,000 keys and
   SurrealDB removed **0**; `purge-orphans` shared the same `record_key` and would have deleted
   nothing against any real key too. The fixture tests were green because they built ids by hand
   (`format!("property:orphan-{i}")`) and so never round-tripped a quoted key. Use the shipped
   `record_key` in [`crates/cli/src/commands/purge.rs`](../crates/cli/src/commands/purge.rs),
   which strips backticks and `⟨⟩` and splits on the **first** colon only, so colons inside a
   quoted key stay part of the key. Note what caught this: rule 1's fail-loud count check. Without
   it the purge would have reported success having deleted nothing.
9. **Page every large read.** The SurrealDB 3.0.x server / `3.1.0-beta.2` SDK pairing cannot
   wire-decode a large response — roughly 55k rows decode, ~443k do not. Past that ceiling the SDK
   returns `Failed to deserialize flatbuffers payload: Failed to decode fb value`. Unlike Standing Rules 2
   and 4 this one fails *loudly*, but it is no less damaging mid-run: an unpaged
   `SELECT node_id, name FROM icij_offshore_entity LIMIT 5000000` over 814,315 rows aborted the
   OCOD re-emit on 2026-08-22 **after** the D-14 purge had already deleted the superseded events,
   leaving the analysis with no OCOD ownership until the re-emit was fixed and re-run. Route reads
   through `urban_integrity_core::persistence::scan` rather than hand-rolling a `LIMIT`. See
   [Reads too large to decode](#reads-too-large-to-decode).

10. **A size cap must be enforced on bytes actually received, never on a number the peer supplied.**
    `Response::content_length()` is `Option<u64>`: absent under chunked transfer-encoding, and
    server-controlled in every case, so an `if let Some(len) = …` pre-check followed by
    `resp.bytes().await?` applies **no cap at all** when the header is missing or understated —
    which was the state of three of the five bulk adapters until 2026-08-23. Route bulk downloads
    through `stream_to_file_capped` / `read_to_vec_capped` in
    [`crates/enricher/src/adapters/download.rs`](../crates/enricher/src/adapters/download.rs), which
    count bytes as they arrive and abort past the cap (the `charity_commission` and `icij` adapters
    enforce the same rule inline). A `content_length()` pre-check is still worth keeping — it avoids
    *starting* a declared-oversize download — but it is an optimisation, not the guard. The same
    reasoning applies to the sidecar's `follow_redirects` SSRF control: a guard that a remote party
    can switch off is not a guard.
11. **Read a `graph_events` payload through the shipped helper, not a hand-rolled path.**
    `weight_json` holds a serialised `NodeWeight` tagged `{"type": …}` — there is no `data` key and
    never has been. Two detectors indexed `["data"]["status"]["type"]` and fell through to
    `.unwrap_or("Active")`, so `property_accumulation.rs`'s "skip InLiquidation / Dissolved" gate
    never skipped anything and `evasion.rs` treated every entity as active. The same pair compared
    node ids with a bidirectional `contains`, turning an identifier comparison into a prefix match
    (`director:john` matched `director:john-smith`, attributing one person's contracts to another).
    Use `node_status::entity_is_active` and `node_id::canonical_entity_id`
    (`crates/enricher/src/`) — canonicalise once, then compare exactly, per the exact-match-only
    policy.
12. **`.len()` on an event scan is not a population.** `graph_events` is append-only, so
    re-ingesting an entity emits another `NodeAdded` row.
    `scan::scan_entities_by_type` returns one entry per **event**;
    `scan::scan_latest_entities_by_type` collapses to one entry per **entity** (greatest
    `timestamp` wins). Reporting the first as a population is defect **ADJ-01**:
    `probe/geographic_concentration.rs` did exactly that and counted **102,897** `Location`
    events as **61,756** distinct Locations, so 22,839 duplicated entities each crossed the
    "two applications on one street" threshold on their own duplicates. Fixed in `c8fb0c7`. See
    [Counting events versus counting entities](#counting-events-versus-counting-entities).
13. **Never deduplicate the two population-counting paths.**
    `crates/core/src/persistence/oracle.rs` must not import or call anything from
    `persistence::scan` — requirement **ORC-06**, enforced by
    `crates/core/tests/oracle_independence_guard.rs`, which fails the build on a `scan::` call or
    a `persistence::scan` mention in code. The duplication *is* the mechanism: a defect in one
    path surfaces as a disagreement with the other instead of two modules silently agreeing on a
    wrong number. The same instruction sits on `scan::latest_entity_weights` and on
    `export_graphml.rs`'s two copies of the same dedup rule. See
    [Two-Path Verification](#two-path-verification-the-population-oracle).
14. **Run one cargo invocation at a time on a shared workstation.** `jobs = 8` in a user's
    `~/.cargo/config.toml` caps a *single* invocation and does nothing about two of them, and
    rustc on this workspace peaks at roughly 3 GB per process. Overlapping builds (a backgrounded
    `cargo build`, a `cargo clippy`, a scoped `cargo test`, plus `scripts/toy-analysis.sh` and
    `scripts/build-workbench.sh`, both of which count as cargo invocations) have driven a 31 GB
    machine into reclaim livelock and required a power cycle. If a cargo command is still running
    in the background, wait for it to exit before starting another. See [Build
    Concurrency](#build-concurrency--one-cargo-at-a-time).

Two more that are enforced by construction rather than discipline: the **exact-match-only** entity
resolution policy (no in-crate fuzzy matching — see the enricher invariants below) and the
**temporal edge** contract (every edge carries `created_at` / `effective_from` / `effective_to`).

---

## Project Structure

```
urban-integrity/
├── Cargo.toml              # Workspace root
├── rust-toolchain.toml     # Pinned toolchain (1.96.0) — single source of truth
├── config.toml             # Runtime configuration
├── .env                    # Local secrets (not committed — gitignored)
├── .env.example            # Template for .env (committed)
├── scripts/
│   ├── surreal-start.sh       # Launch isolated, memory-capped SurrealDB (rocksdb://) as a user service
│   ├── surreal-guard.sh       # Shell guard: intercepts bare `surreal start`, routes to surreal-start.sh
│   ├── surreal-drop-test-ns.sh # Sweep `test*` namespaces stranded by a SIGKILLed test binary
│   ├── sidecar-start.sh       # Launch the Scrapling planning sidecar (:8765) as a user service
│   ├── build-workbench.sh     # The ONE place Trunk runs: builds workbench-web → crates/workbench/dist/
│   ├── toy-analysis.sh        # Provision, seed, project and probe the planted fixture analysis
│   ├── deploy-demo.sh         # Ship a fixture-only workbench demo to a remote host (no corpus data)
│   ├── check-wasm-boundary.sh # CI guard: no native-only crate enters the wasm32 dependency graph
│   ├── check-verdict-deserialize.sh  # CI guard: no `Deserialize` on verdict-domain types
│   ├── check-no-analysis-literals.sh # CI guard: no analysis-specific literals in workbench source
│   ├── check-fixture-not-shipped.sh  # CI guard: planted-graph assembler stays test-only
│   ├── check-abbreviations.sh # CI guard: workbench-web abbreviations are glossed or allowlisted
│   ├── abbreviations-allowlist.txt   # Allowlist read by check-abbreviations.sh
│   ├── check-shim-drift.sh    # CI guard: vendored Cosmograph shim matches its source bundle
│   ├── check.sh               # One-shot local CI gate (fmt, clippy, tests — mirrors ci.yml)
│   ├── status.sh              # Read-only "where are we?" snapshot (commits, findings, worklog)
│   ├── report.sh              # End-to-end report runner (wraps `cargo run -p urban-integrity-cli`)
│   ├── enrich.sh              # Run OSINT enrichment adapters (wraps cargo run enrich)
│   ├── export_graphml.sh      # Build + export graph_events WAL to timestamped GraphML file
│   ├── export_parquet.sh      # export-graphml, then convert to Cosmograph-ready node/link Parquet
│   ├── rebuild-anglesey.sh    # Maintainer-specific: refresh the reference analysis's derived layer
│   ├── surreal_target.py      # Shared --analysis / URBAN__ANALYSIS resolution for the Python scripts
│   ├── analyst_probe.py       # Adaptive forensic battery → generates evidence-report.md
│   ├── projection_audit.py    # Is each projected table TRUNCATED / STALE / DROPPED / COMPLETE?
│   ├── build_deprivation_ward_index.py  # Build the per-nation ward deprivation index (data/ is gitignored)
│   ├── phoenix_funnel.py      # Where Phoenix candidates die, measured against graph_events
│   ├── ch_officer_coverage.py # Close Phoenix's real gap: CF awardees carrying no Directs edge
│   ├── repair_psc_officer_links.py  # Repoint ch_psc identity links whose officer target no longer exists
│   ├── corroborate_offsite_applicant_matches.py  # Second signal for withheld applicant→company matches
│   ├── pa_ingest.py           # Ingest Sandwell councillors as PoliticalActor nodes
│   ├── pa_link_officers.py    # Link PA nodes to CH officer records (enables NepotismBridge)
│   ├── ch_backfill.py         # Resolve applicant names to CH companies + officers
│   ├── ch_cessation_backfill.py  # Fetch real date_of_cessation from CH REST (Phoenix coverage)
│   ├── cf_backfill_dates.py   # Backfill date_published on CF contracts from OCDS release.date
│   ├── resolve_planning_applicants_to_ch.py  # Give planning applicants a real CH identifier
│   ├── fix_edge_directions.py # Add forward GB-COH→OFF edges for Phoenix traversal
│   ├── normalize_officer_keys.py  # Create shared OFF:{name_slug} nodes for cross-company detection
│   ├── rekey_officers_to_director_slug.py  # Re-key OFF:{company}-{NAME} → director:{slug}
│   ├── officer_merge_audit.py # Adjudicate name-only officer merges against CH REST (officer_id + DOB)
│   ├── split_false_merged_officers.py  # Split officer nodes the name-only re-key wrongly fused
│   ├── land_flip_funnel.py    # Measure the land-flip funnel gate by gate (calls the shipped Rust shims)
│   ├── land_flip_pairable.py  # Count planning/PPD pairs that are structurally matchable at all
│   ├── prune_isolated_nodes.py   # Streaming removal of degree-zero nodes from a GraphML export
│   └── wards                  # Maintainer-specific: the reference analysis's 14 ward codes
├── crates/
│   ├── schema/             # Shared types (node, edge, ID) — no business logic
│   │   └── src/types/      # node.rs, edge.rs, id.rs, verdict.rs (Verdict / Coverage /
│   │                       # Finding — the probe verdict vocabulary; Serialize only,
│   │                       # deliberately never Deserialize)
│   ├── test-support/       # Shared SurrealDB test harness: TestNs, NsGuard, and planted.rs —
│   │                       # the planted fixture graph + its literal Manifest. A dev-dependency
│   │                       # everywhere except cli, which uses it for `fixture seed --planted`
│   ├── core/               # Graph engine, WAL, persistence, config
│   │   ├── src/graph/      # storage.rs, snapshot.rs, merge.rs, signals.rs, types.rs,
│   │   │                   # coordinator.rs (also stamps the detector_runs marker on the
│   │   │                   # detector's success path only — never a motif_count: 0 row
│   │   │                   # standing in for "the detector never ran")
│   │   ├── src/stream/     # CursorStore trait, SurrealStreamCursor, run_ch_stream,
│   │   │                   # normalise_name — shared CH streaming primitives.
│   │   │                   # Extracted here so enricher can use CH streams without
│   │   │                   # importing the ingestor's clients/pollers (ENR-01)
│   │   ├── src/analysis/   # naming.rs — pure analysis-naming module (slugify,
│   │   │                   # wardset_hash, analysis_db_name, checkpoint_path_for)
│   │   ├── src/persistence/# surreal.rs (AIMD BatchController), scan.rs (row-safe paged
│   │   │                   # scans over graph_events, identity_links and
│   │   │                   # land_flip_candidates), oracle.rs (the INDEPENDENT
│   │   │                   # count()/GROUP BY population path — must never call
│   │   │                   # scan.rs; ORC-06), detector_runs.rs (the
│   │   │                   # detector_runs:current marker reader/writer),
│   │   │                   # land_flip_runs.rs, land_flip_capability.rs,
│   │   │                   # checkpoint.rs, dead_letter.rs, queries.rs,
│   │   │                   # traversal.rs, wal.rs
│   │   ├── src/bin/        # normalise-name.rs — stdin/stdout shim exposing the SHIPPED
│   │   │                   # normalise_name to out-of-process scripts
│   │   ├── benches/        # macro_bench.rs (Criterion — end-to-end), concurrent_readers.rs
│   │   ├── examples/       # latency_monitor.rs
│   │   └── tests/          # hybrid_validation.rs, query_shape_guard.rs, performance.rs,
│   │                       # surreal_integration.rs, coordinator_update_node.rs,
│   │                       # contract_row_fields.rs, replay_status_transition.rs,
│   │                       # upsert_status_transition.rs, detector_run_marker.rs,
│   │                       # entity_query_status_fold.rs, oracle_shapes.rs,
│   │                       # oracle_independence_guard.rs (the ORC-06 source guard)
│   ├── ingestor/           # Data source clients
│   │   ├── src/ch/         # Companies House streaming client (ChStreamClient)
│   │   │                   # Uses connect_timeout (not timeout) — long-lived NDJSON stream
│   │   ├── src/cf/         # Contracts Finder REST poller
│   │   │   └── mapper.rs   # OCDS release → IngestEvents; tender_ref from tender.id,
│   │   │                   # date_published from the mandatory release.date
│   │   ├── src/address/    # Address normalisation pipeline
│   │   │   └── planning.rs # Free-text planning-address parser — bilingual " / " split FIRST,
│   │   │                   # then commas (D-07/D-25)
│   │   ├── src/planning/   # Planning portal pipeline
│   │   │   ├── sidecar.rs  # Scrapling sidecar HTTP client
│   │   │   ├── firecrawl.rs# Firecrawl fallback client
│   │   │   ├── mapper.rs   # PlanningPortalItem → IngestEvents; application_ref, applicant_slug
│   │   │   └── poller.rs   # PlanningPoller background Tokio task
│   │   ├── src/projector.rs# CQRS projection: graph_events WAL → native SurrealDB tables
│   │   ├── src/bin/        # applicant-slug.rs — stdin/stdout shim exposing the SHIPPED
│   │   │                   # applicant_slug (director:{slug} key function)
│   │   └── src/backfill/   # Backfill runners
│   │       ├── ch.rs       # Link planning applicants to CH company numbers
│   │       ├── ward.rs     # Resolve contract postcodes to ward codes via postcodes.io
│   │       ├── planning.rs # Repair unknown-applicant attribution; stamp applicant provenance
│   │       ├── cursor.rs   # Resume cursors for backfills whose cost is external (CH API budget)
│   │       ├── session.rs  # DbSession — re-auth wrapper surviving the ~1h SurrealDB token expiry
│   │       └── mod.rs      # Shared helpers (scan_events_by_type, update_node_event_by_record, …)
│   ├── stats/              # EMPTY. Untouched `cargo new` template — `add(left, right)`
│   │                       # and its `it_works` test, 14 lines. `api` and `cli` both declare
│   │                       # a dependency on it; nothing in the workspace references it.
│   │                       # Scoring lives in detector/src/scoring/, NOT here.
│   ├── detector/           # Motif detectors, alert cache
│   │   ├── src/motif/      # phoenix.rs, nepotism.rs, cluster.rs
│   │   ├── src/scoring/    # formula.rs, imd.rs, mod.rs, registry.rs
│   │   ├── src/alert/      # result.rs (MotifResult schema), cache.rs
│   │   └── benches/        # motif_detectors.rs (Criterion — micro)
│   ├── enricher/           # OSINT enrichment engine
│   │   ├── src/adapters/   # EnricherAdapter trait + fourteen adapters:
│   │   │   ├── ch_psc.rs              # PSC beneficial ownership (CH Streaming API)
│   │   │   ├── ch_disqualified.rs     # Disqualified Directors (CH Streaming API)
│   │   │   ├── land_registry_ccod.rs  # HMLR CCOD bulk CSV (monthly, PROP-01)
│   │   │   ├── land_registry_ocod.rs  # HMLR OCOD overseas ownership (PROP-05/06)
│   │   │   ├── land_registry_ppd.rs   # HMLR Price Paid Data staging (FLIP-01)
│   │   │   ├── london_gazette.rs      # Corporate insolvency notices (PROP-02, public API)
│   │   │   ├── charity_commission.rs  # Charity Commission bulk register (FUZ-02, OGL)
│   │   │   ├── opensanctions.rs       # OpenSanctions/yente PEP + sanctions (FUZ-03, CC-BY-NC)
│   │   │   ├── icij.rs                # ICIJ Offshore Leaks (FUZ-04, ODbL isolated tables)
│   │   │   ├── electoral_commission.rs# EC donations CSV (POL-01, OGL)
│   │   │   ├── council_registers.rs   # Council Registers of Interest via sidecar (POL-02)
│   │   │   ├── opencorporates.rs      # OpenCorporates cross-jurisdiction lookup (POL-03, ODbL)
│   │   │   ├── planning_identity.rs   # Planning applicant ↔ CH entity exact-name join (IDN-05)
│   │   │   ├── mutuals_register.rs    # FCA Mutuals Public Register — registered societies
│   │   │   └── download.rs            # NOT an adapter: the size-capped body readers the bulk
│   │   │                              # adapters share — cap enforced on bytes RECEIVED
│   │   ├── src/evasion.rs             # Disqualified Director Evasion detector
│   │   ├── src/property_accumulation.rs # Property Accumulation Anomaly detector (PROP-03)
│   │   ├── src/offshore_shadow.rs     # Offshore Shadow Company detector (FUZ-05)
│   │   ├── src/charity_crossover.rs   # Charity-Commercial Crossover detector (FUZ-06)
│   │   ├── src/donation_correlation.rs# Political Donation → Contract Correlation (POL-04)
│   │   ├── src/ec_date.rs             # Shared Electoral Commission AcceptedDate parser
│   │   │                              # (parse_ec_date, to_iso) — both EC call sites route here
│   │   ├── src/land_flip.rs           # Pre-Permission Land Flip detector (FLIP-02..05)
│   │   ├── src/country_canonical.rs   # Country-of-incorporation canonicalisation (OCOD)
│   │   ├── src/node_id.rs             # canonical_entity_id — one node-id form out of a row,
│   │   │                              # so detectors compare exactly instead of by `contains`
│   │   ├── src/node_status.rs         # entity_is_active — reads status out of weight_json's
│   │   │                              # real shape (there is no `data` key; Standing Rule 11)
│   │   ├── src/confidence.rs          # ConfidenceScore, ConfirmedLink, CandidateLink, SubCandidateLink, LinkStatus
│   │   ├── src/engine.rs              # EnricherEngine + build_default_adapters()
│   │   ├── src/wal_writer.rs          # graph_events WAL writer (write_wal_event, build_wal_record_id, drain_to_wal)
│   │   ├── src/bin/                   # gate1-address-key.rs — stdin/stdout shim exposing the
│   │   │                              # SHIPPED land-flip gate-1 address key to the funnel harness
│   │   └── tests/                     # ENR-01/ENR-08/ENR-11 compliance + regression tests:
│   │                                  # hot_path.rs, engine_coverage.rs, psc_adapter.rs,
│   │                                  # disqualified_adapter.rs, ccod_adapter.rs,
│   │                                  # gazette_adapter.rs, evasion_detector.rs,
│   │                                  # property_accumulation.rs, jurisdiction.rs,
│   │                                  # donation_correlation_endpoint.rs,
│   │                                  # opencorporates_targets.rs
│   ├── api/                # gRPC + GraphQL server
│   │   ├── proto/          # integrity.proto
│   │   └── src/graphql/    # schema.rs (async-graphql)
│   ├── cli/                # clap CLI binary (urban-integrity-cli)
│   │   ├── src/commands/   # report.rs, alerts.rs, anomalies.rs, status.rs, entity.rs,
│   │   │                   # ingest_planning.rs, graph_path.rs, project.rs, backfill.rs,
│   │   │                   # export_graphml.rs, enrich.rs, links.rs, verify.rs, flips.rs,
│   │   │                   # migrate.rs, purge.rs, analysis.rs, fixture.rs, probe.rs
│   │   │                   # (thin wrapper over crates/probe), probe_oracle.rs,
│   │   │                   # workbench.rs (serves the analyst workbench)
│   │   ├── src/lib.rs      # Re-exports for integration tests + detector_config_from()
│   │   ├── src/db.rs       # connect_db + resolve_analysis_db (analysis-aware connection)
│   │   ├── src/registry.rs # _catalog registry access (analyses table)
│   │   └── tests/          # report_md.rs, report_pdf.rs, cli.rs, osint_surface.rs,
│   │                       # analysis_lifecycle.rs, probe_tier1_e2e.rs, probe_tier2_e2e.rs,
│   │                       # probe_oracle_agreement.rs, evidence_report_*.rs,
│   │                       # toy_analysis_chain.rs, workbench_boot.rs,
│   │                       # purge_delete_spike.rs, … — CLI integration tests
│   ├── probe/              # The 22-probe forensic battery, extracted from cli so the
│   │   │                   # workbench can run it in-process
│   │   ├── src/lib.rs      # Probe trait, Strategy, run_probes (the ungated primitive)
│   │   ├── src/run.rs      # execute(), build_battery (the load-bearing registration order)
│   │   ├── src/graph.rs    # Dependency-graph validation + run_gated (verdict gating)
│   │   ├── src/report/     # evidence-report.md rendering
│   │   ├── src/*.rs        # one file per probe
│   │   └── tests/          # golden_report.rs, findings_coverage.rs, motif_kind_coverage.rs,
│   │                       # planted_evidence_refs.rs
│   ├── workbench-dto/      # Wire-format view types shared by server and browser. serde only —
│   │                       # the ONLY workspace crate the wasm frontend may depend on for data
│   ├── workbench/          # Native (server) half of the analyst workbench: Axum handlers,
│   │   │                   # replay-only boot, population/graph algorithms (graphrs, arrow),
│   │   │                   # in-process probe runs, rust-embed static-asset serving
│   │   ├── src/assets.rs   # rust-embed over dist/ with SPA fallback and a "not built" stub
│   │   └── dist/           # Trunk output (gitignored) — produced by scripts/build-workbench.sh
│   └── workbench-web/      # Leptos 0.8 CSR frontend, compiled to wasm32-unknown-unknown
│       ├── Trunk.toml      # dist = "../workbench/dist" (lands straight where rust-embed reads)
│       ├── index.html      # Trunk entry point
│       ├── js/             # Vendored Cosmograph ESM shim (cosmograph-shim.js + .sha256)
│       ├── js-build/       # npm/esbuild project that regenerates the shim (not a cargo concern)
│       ├── style/          # Stylesheets
│       ├── src/            # Pages, components, routing (main.rs, app.rs, …)
│       └── tests/          # Native reachability/render tests (abbr_reachable.rs, routes_render.rs, …)
├── scrapling-sidecar/      # Python FastAPI scraping sidecar (NOT a Cargo workspace member)
└── docs/                   # Documentation
```

---

## Toolchain

The Rust toolchain is **pinned to `1.96.0`** in `rust-toolchain.toml` at the repo root. This file is the single source of truth shared by local development and CI:

```toml
[toolchain]
channel = "1.96.0"
components = ["clippy", "rustfmt"]
profile = "minimal"
targets = ["wasm32-unknown-unknown"]
```

`rustup` auto-installs this exact version (with `clippy`, `rustfmt` and the
`wasm32-unknown-unknown` target the workbench frontend compiles to) on the first `cargo`/`rustc` invocation inside the workspace — locally and on GitHub runners alike. You do not need to install or select a toolchain by hand.

**Why pin (don't track `stable`):** clippy's lint set changes between releases. The CI clippy gate runs `-D warnings`, so a contributor on a different toolchain than the runner can pass `cargo clippy` locally yet fail in CI (or vice versa). This happened on 2026-06-20: local `1.90.0` missed lints (`unnecessary_sort_by`, `useless_vec`, `collapsible_match`) that `1.96.0` flagged. Pinning ends that drift.

**Bumping the toolchain** is a deliberate act: edit the `channel` in `rust-toolchain.toml`, then re-run the clippy gate (`cargo clippy --workspace --all-targets -- -D warnings`) and fix any newly surfaced lints in the same change. Do not switch the channel to `stable`.

**The MSRV is intent, not enforcement.** No manifest in the workspace declares a `rust-version` key — not the workspace root, not any of the thirteen crates — so the documented MSRV of **1.87+** constrains nothing cargo checks. Nothing fails, locally or in CI, if a change starts depending on a newer feature. `rust-toolchain.toml`'s `1.96.0` is the only version anything actually enforces. If the MSRV matters for a change, verify it deliberately (build against that toolchain yourself) rather than trusting a green build to have told you.

---

## Build Concurrency — One Cargo at a Time

**Run one cargo invocation at a time on a shared workstation.** `jobs = 8` in a user's
`~/.cargo/config.toml` caps the parallelism of a *single* invocation; it does nothing about two
invocations running side by side, and rustc on this workspace peaks at roughly 3 GB per process.
That cap is a per-user setting, not committed to the repo, so fresh clones, worktrees and CI
runners have none.

The failure this rule exists for: three overlapping cargos — a `cargo build --workspace --tests`
that outran a tool timeout and kept running in the background, a `cargo clippy --workspace
--all-targets`, and a scoped `cargo test -p urban-integrity-workbench` — alongside
`scripts/toy-analysis.sh` and `scripts/build-workbench.sh` in a terminal, put 41 descendant
processes and ~17.4 GB against a 31 GB machine whose daemons already held ~8.5 GB. Free memory
hit 390 MB, the desktop compositor stalled for minutes and the machine had to be power-cycled.
Nothing was OOM-killed — the box livelocks in reclaim rather than shedding load.

- If a cargo command is still running (including one moved to the background by a timeout),
  wait for it to exit before starting another — not clippy, not fmt, not a scoped `-p` test.
- `scripts/toy-analysis.sh` (several `cargo run`s) and `scripts/build-workbench.sh`
  (`trunk build --release`, which drives cargo for the wasm target) **count as cargo
  invocations**.
- When someone else may be building on the same machine, say what you are about to build before
  you start it.
- To diagnose a suspected freeze after the fact:
  `journalctl -b -1 | grep -E 'Under memory pressure|Pageflip|NVRM'`.

---

## Common Commands

```bash
# Build
cargo build --workspace

# Build specific crate
cargo build -p urban-integrity-detector

# Run all tests (no external services)
cargo test --workspace

# Run tests for one crate
cargo test -p urban-integrity-core

# Run one integration test binary (requires no external services)
cargo test -p urban-integrity-core --test hybrid_validation

# Run full suite including SurrealDB integration tests
# Requires SurrealDB running at ws://localhost:8000 — start it via the isolated launcher:
#   scripts/surreal-start.sh
URBAN_DB_TESTS=1 cargo test --workspace

# One-shot local CI gate: fmt --check, clippy -D warnings, workspace tests
scripts/check.sh                 # add --db for the SurrealDB-gated suite, --fix to fmt first

# Sweep `test*` namespaces stranded by a SIGKILLed test binary (dry run by default)
scripts/surreal-drop-test-ns.sh
scripts/surreal-drop-test-ns.sh --apply

# Run with coverage (HTML report)
URBAN_DB_TESTS=1 cargo llvm-cov --workspace --html

# Check without building artifacts (fast feedback)
cargo check --workspace

# Fix warnings automatically
cargo fix --lib -p urban-integrity-detector

# Run CLI in dev mode
cargo run -p urban-integrity-cli -- report --min-score 0.5

# Run the 22-probe forensic battery against the configured analysis
cargo run -p urban-integrity-cli -- probe

# Cross-check every checkable probe's declared population against an independent count
cargo run -p urban-integrity-cli -- probe-oracle

# End-to-end report helper (starts SurrealDB + engine, runs report, shuts down)
./scripts/report.sh --ward X05999001 --min-score 0.7

# Build the workbench frontend (Trunk → crates/workbench/dist/), then rebuild the binary
# that embeds it — see the Analyst Workbench section
scripts/build-workbench.sh
touch crates/workbench/src/lib.rs && cargo build -p urban-integrity-cli

# Seed the planted fixture analysis and serve the workbench on 127.0.0.1:8090
scripts/toy-analysis.sh
cargo run -p urban-integrity-cli -- workbench --analysis <printed slug>

# Compile the browser crates for the real wasm target (what CI's wasm-build job checks)
cargo build --target wasm32-unknown-unknown -p urban-integrity-workbench-dto -p urban-integrity-workbench-web
```

Every command in this block is a cargo invocation (or wraps one) — run them one at a time. See
[Build Concurrency](#build-concurrency--one-cargo-at-a-time).

> **Budget real time for the DB-gated suite.** `URBAN_DB_TESTS=1 cargo test --workspace` is not a
> seconds-long command locally. `crates/cli/tests/cli.rs::test_cli_report_markdown_creates_file`
> spawns the CLI with no `--ward`, no `--limit` and no `--analysis`, so it runs a **full-corpus
> `report`** against the default database from `config.toml` — in a **debug** build. On a fresh
> clone that database is empty and the run is quick; the timings below are from a maintainer store
> holding a real corpus. That one binary dominates the run: measured 2026-09-16, `crates/cli/tests/cli.rs`
> took **~440 s** gated against **0.13 s** ungated, for the same 44 tests, and a whole-workspace
> gated run took **637 s** wall. CI does not see this because its SurrealDB is a fresh in-memory
> instance with nothing in it. If you only need fast feedback, scope the run — `cargo test -p
> urban-integrity-core`, or `URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test cli
> <name-filter>` — and reserve the full workspace run for the pre-merge gate.

---

## Formatting & Linting

`rustfmt` and `clippy` use compiler defaults — there is no `rustfmt.toml` or `clippy.toml`, and no project-level lint configuration.

```bash
# Format the whole workspace
cargo fmt --all

# Check formatting without writing (CI-style)
cargo fmt --all -- --check

# Lint the whole workspace, treating warnings as errors
cargo clippy --workspace --all-targets -- -D warnings
```

`scripts/check.sh` runs all the CI gates locally in one shot — `fmt --all --check`, `clippy -D warnings`, and the workspace tests — running **every** gate even when an earlier one fails (avoiding the `fmt && clippy` short-circuit foot-gun), then prints a summary and exits non-zero if any gate failed. Flags: `--fix` (run `cargo fmt --all` first), `--db` (also run the `URBAN_DB_TESTS=1` suite). Full logs go to `/tmp/urban-check-<timestamp>.log`.

The workspace is built on the toolchain pinned in `rust-toolchain.toml` (**Rust `1.96.0`**; see the Toolchain section above) on the 2021 edition — each crate's `Cargo.toml` declares `edition = "2021"`, and none declares a `rust-version`. The documented MSRV of **1.87+** is therefore an intent, not a constraint cargo enforces: nothing fails if a change starts depending on a newer feature. Local dev and CI both build on the pinned `1.96.0`, so the clippy lint set cannot drift between them; if the MSRV matters to you, verify it deliberately rather than trusting the build.

---

## Continuous Integration

CI is defined in `.github/workflows/ci.yml`. It runs on every push to `main` and on every pull request. There are three jobs — `test`, `lint` and `wasm-build` — and **all must pass** before a change merges. All install the pinned toolchain the same way local dev gets it — via `rustup show`, which triggers rustup's auto-install of the version in `rust-toolchain.toml` inside the workspace.

### `test` job — Build + full suite (incl. SurrealDB integration)

Runs on `ubuntu-latest` and exercises the **DB-gated** integration suite that a plain `cargo test` skips:

1. Install `protoc` (`protobuf-compiler`) — required by `crates/api/build.rs`, which compiles `proto/integrity.proto` at build time.
2. Install the pinned toolchain via `rustup show`, then restore the cargo cache
   (`Swatinem/rust-cache@v2`).
3. Start SurrealDB via `surrealdb/setup-surreal@v2`, pinned to `v3.1.0-beta.2` (matching the `=3.1.0-beta.2` SDK in workspace deps), on port `8000` with `root`/`root`, and **with `surrealdb_auth: true`**. The action uses its default in-memory engine — fine for the suite, which seeds tiny fixtures in throwaway per-run namespaces. The dev/prod engine is RocksDB.

   > **Why `surrealdb_auth: true` is set explicitly.** The action defaults that input to
   > `false`, which passes `--unauthenticated` to the server: every request is then accepted as
   > root regardless of the credentials sent. Local dev runs auth-on (`scripts/surreal-start.sh`),
   > so CI and local diverged on exactly the behaviour several tests exist to check —
   > `db_query_recovers_from_a_dead_session` invalidates a session and asserts the raw connection
   > then fails, and the `ch_psc` / `ch_disqualified` "no signin" regressions assert that a write
   > is **refused**. Under `--unauthenticated` none of those can fail for the reason claimed: they
   > were green without testing anything. Keep CI's auth posture matching local.
4. Build the workspace (`cargo build --workspace`) as its own step, so a compile failure is reported as a build failure rather than buried in the test step's output.
5. Pre-initialise the schema once with `cargo run -p urban-integrity-cli -- status` (under `URBAN_DB_TESTS=1`). `connect_db()` applies the schema on every connect; pre-seeding makes the many CLI subprocess tests' DDL no-ops so they don't race into a transaction conflict.
6. Run the full suite: `URBAN_DB_TESTS=1 cargo test --workspace` — the DB-gated integration tests run here so they can't silently rot (gated tests have broken unnoticed before). The gate is a skip-**by-return**, so a green `cargo test --workspace` without the env var reports the same passing count while exercising none of them.

Two workflow-level `env` knobs keep the job stable on the 2-core runner:

- `RUSTFLAGS: "-C debuginfo=0"` — the enricher test binary statically links the whole `surrealdb-core` + crypto + geo tree; full debug info exhausts runner memory and the linker dies (SIGBUS). Tests only need pass/fail, so debug info is dropped.
- `RUST_TEST_THREADS: "2"` — caps per-binary test parallelism so the CLI integration tests (each spawning a subprocess that hits the shared SurrealDB) don't contend and flake.

Live external tests (`URBAN_LIVE_TESTS=1`, which hit Companies House / Contracts Finder) are intentionally **not** run in CI — they need a CH REST key as a secret and depend on third-party availability. Run them locally before merging changes to the ingestor HTTP clients or mappers.

### `lint` job — rustfmt + clippy

Also on `ubuntu-latest`. Installs `protoc` (clippy compiles the workspace, so `build.rs` still needs it) and the pinned toolchain, then runs the two gates:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`-D warnings` makes any clippy lint a hard failure. The workspace is clippy-clean; keep it that way (fix the lint, or `#[allow]` with a justification). Because the toolchain is pinned, running these two commands locally reproduces the CI lint result exactly. `scripts/check.sh` runs the same gates locally.

### `wasm-build` job — Wasm frontend build + boundary guards

Also on `ubuntu-latest`. The `test` and `lint` jobs only ever compile for the native target, which
proves nothing about whether the browser crates are wasm-safe. This job does:

1. Install the pinned toolchain via `rustup show` — `rust-toolchain.toml` lists
   `wasm32-unknown-unknown` under `targets`, so no separate `rustup target add` is needed.
2. Install **Trunk 0.21.14** (`cargo binstall`, falling back to `cargo install --locked`).
3. `cargo build --target wasm32-unknown-unknown -p urban-integrity-workbench-dto -p urban-integrity-workbench-web`.
4. Run the source guards, each of which fails the job:

   | Script | Fails when |
   |--------|-----------|
   | `scripts/check-wasm-boundary.sh` | `surrealdb`, `tokio`, `petgraph`, `rayon`, or `crates/schema` / `core` / `detector` enters the wasm32 dependency graph of `workbench-dto` or `workbench-web` |
   | `scripts/check-verdict-deserialize.sh` | `Deserialize` reappears on the schema crate's verdict type, or on a bare-named verdict-domain type in a workbench crate |
   | `scripts/check-no-analysis-literals.sh` | An analysis-specific string leaks into workbench source |
   | `scripts/check-fixture-not-shipped.sh` | `workbench_from_graph` (the test-only planted-graph assembler) is referenced outside its definition, the workbench/CLI test trees, and prose |
   | `scripts/check-abbreviations.sh` | A `workbench-web` string literal contains an unexpanded 2–6 letter all-caps abbreviation that is neither glossed in `docs/abbreviations.md` nor listed in `scripts/abbreviations-allowlist.txt` |
   | `scripts/check-shim-drift.sh` (with `SHIM_DRIFT_STRICT=1`) | The vendored `crates/workbench-web/js/cosmograph-shim.js` no longer matches a fresh bundle of `js-build/cosmograph-shim-src.js`. Needs Node 20 and `npm ci` in `crates/workbench-web/js-build`; strict mode makes a missing toolchain a failure rather than a skip |

5. `cd crates/workbench-web && trunk build --release` — `dist/` is never committed, so this proves
   the frontend actually bundles with Trunk, not just that it compiles.

`scripts/check.sh` does **not** run this job's steps. To reproduce it locally, run the wasm32
`cargo build` above, the guard scripts, and `scripts/build-workbench.sh` — one at a time.

### Measured gate status

Last measured **2026-09-16** on `main`, against a store holding 986,062 `graph_events`:

| Gate | Result |
|------|--------|
| `URBAN_DB_TESTS=1 cargo test --workspace` | **2,573 passed / 0 failed / 11 ignored**, ~637 s wall |
| `URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test cli` | 44 passed, ~430–465 s across five runs |
| `cargo test -p urban-integrity-cli --test cli` (ungated) | the same 44 passed, 0.13 s |

**The gated and ungated pass counts are identical on purpose, and that is the trap.** The DB gate
is a skip-**by-return**, not a `#[ignore]`, so a skipped test still counts as passed. Both commands
report the same total whether or not a single DB test did any work. The only observable
difference is *duration*. Never read a green `cargo test --workspace` as evidence that the
DB-gated tests ran.

Treat **~450 s** for the gated CLI binary and **~10–15 minutes** for a whole-workspace gated run
as the shape of a healthy run, warm or cold. An earlier figure of ~1,400 s for the CLI binary
(and ~1,340 s for the workspace) was roughly 3x too slow; a controlled restart that dropped
SurrealDB's caches showed cache warmth accounts for none of the gap — cold, one-hour-warm and
two-days-warm runs landed within 2.4% of each other. The slow runs coincided with heavy memory
pressure on the host, which is a correlation, not a demonstrated cause: when a gated run comes in
far slower than ~450 s, look at what else the machine is doing before suspecting the database.

**A *fast* gated run is the thing to distrust — a run that finishes in seconds, not minutes.**
Once, this section was "corrected" to claim the CLI binary ran in 1.79 s gated. That measurement
had been taken against a SurrealDB launched from inside a **git worktree**, serving an empty
per-worktree store with no `urban_integrity` namespace in it at all. Every count matched, clippy
was clean, and only the duration gave it away. Duration is still the weakest signal; before
trusting a gated timing, confirm the instance is serving the real store:

```bash
# The service must be running from the repo root, not a worktree
systemctl --user show urban-surreal -p WorkingDirectory

# The instance must actually hold the corpus: `INFO FOR ROOT` must list `urban_integrity`

# When the number matters, count the corpus in the exact namespace/database the CLI tests use
curl -s -u root:root -H 'surreal-ns: urban_integrity' -H 'surreal-db: <db_name>' \
  -X POST --data 'SELECT count() FROM graph_events GROUP ALL;' http://localhost:8000/sql
```

**Only the CLI pair is corpus-sensitive** — it spawns the CLI against whatever store the running
instance happens to serve. The `core` integration tests build their own namespaces through
`urban-integrity-test-support`, so they cost the same against an empty store as against the live
one: `--test surreal_integration` is 35 passed in 0.00 s ungated / 5.59 s gated, `--test
oracle_shapes` 6 passed in 0.00 s / 14.11 s (measured 2026-09-03, when it had 6 tests; it now has 7), both stable to within a second either way. Use those
to prove `URBAN_DB_TESTS=1` reached *a* database; use the CLI pair to prove it reached the *right*
one.

The DB-gated subset was last **counted** at 215 on 2026-08-25 and has not been recounted since;
treat that figure as stale until you re-derive it. Count gated tests by
attributing each gate expression to its enclosing `#[test]`/`#[tokio::test]`
function, and note there is more than one gate form: `db_tests_enabled()` from
`crates/test-support`, the local `skip_if_no_db()` helper in `crates/cli/tests/cli.rs`, plus roughly 60 sites that check
`std::env::var("URBAN_DB_TESTS")` directly (`crates/cli/tests/purge_delete_spike.rs`,
`crates/core/tests/entity_query_status_fold.rs`, among others; the
probe crate uses `db_tests_enabled()`). A bare grep for the two helper names both
**over**-counts (their *definitions* match) and **under**-counts (it
misses the raw env checks) — which is why a mechanical recount lands within a couple of tests of
any quoted figure rather than exactly on it.
The heaviest concentrations are `crates/core/tests/surreal_integration.rs` (33),
`crates/cli/tests/cli.rs` (21), `crates/cli/tests/analysis_lifecycle.rs` (17) and
`crates/enricher/tests/engine_coverage.rs` (12). A growing number now live **beside the code**
rather than in `tests/` — `crates/core/src/persistence/scan.rs` carries 8, and each Tier-1 probe
module carries its own.

**Re-measure rather than quoting these numbers.** They drift with every merge, and the figures in
this section have gone stale before and been repeated as fact:

```bash
cargo test --workspace 2>&1 | grep -E '^test result' |
  awk '{p+=$4; f+=$6; i+=$8} END {print "passed", p, "failed", f, "ignored", i}'
```

Reproduce the whole set locally with `scripts/check.sh --db`. A DB must be up
(`scripts/surreal-start.sh`, launched **from the repo root** — see [Never launch from a
worktree](#never-launch-from-a-worktree)) or the DB-gated tests skip by return and the pass count
means nothing about them. Two `#[ignore]`d tests read the operator's local caches under `data/` and do
not run by default — see [TESTING.md](TESTING.md).

---

## Running SurrealDB Locally

Anything that touches the database (the full `URBAN_DB_TESTS=1` suite, `serve`, `enrich`, `project`, `export-graphml`, `analysis`, the CLI lookup commands, and the Python forensic scripts) needs a SurrealDB instance reachable at `ws://localhost:8000` (`ws_url` in `config.toml`).

Start it via the launcher rather than running `surreal start` by hand:

```bash
scripts/surreal-start.sh
```

`scripts/surreal-start.sh` runs SurrealDB as a transient *user systemd service* (`urban-surreal`) under `user.slice`, detached from the calling terminal and with a hard cgroup memory cap (`MemoryMax`, default `12G`) plus a soft reclaim threshold (`MemoryHigh`, default `9G`). Without a systemd user session it falls back to a detached background process with no memory cap (`URBAN_SURREAL_LAUNCHER` forces either). The launcher is idempotent — re-running it is a no-op when a healthy instance is already up. `scripts/report.sh` and `scripts/enrich.sh` both delegate to it, so you usually do not call it directly.

> **Never run `surreal start` by hand.** A bare `surreal start` (e.g. reused from shell history) reintroduces both defects the RocksDB fix removed: it can select the unbounded `surrealkv://` store again, and — launched from a terminal — it inherits the calling shell's cgroup (such as the editor's `app-code-*.scope`), so a DB OOM becomes a *global* OOM that can take the editor down with it. `scripts/surreal-guard.sh` defends against this: source it from `~/.bashrc` and it wraps the `surreal` command, intercepting only the `start` subcommand (every other subcommand — `sql`, `import`, `export`, `version`, …, passes through untouched), warning, and routing to `scripts/surreal-start.sh`. To deliberately bypass the guard for a single call, set `SURREAL_GUARD_BYPASS=1`. On a host with no systemd user session the launcher uses its background mode instead — see [GETTING-STARTED.md step A4](GETTING-STARTED.md#a4-start-surrealdb) — and SurrealDB then has no memory cap.

Storage backend and DB path are owned by the launcher: it uses **RocksDB** at `rocksdb://data/surreal-rocks.db`. RocksDB replaced the former `surrealkv://data/surreal.db` store on 2026-06-17 because surrealkv held every touched key resident in RAM and OOM-killed SurrealDB during the CCOD ingest; the old surrealkv store was kept as a rollback backup until 2026-07-28, then deleted (5.6 GB) once RocksDB had been authoritative for six weeks. Running SurrealDB as an isolated, memory-capped service means a DB OOM is reaped by the cgroup killer alone and cannot take the editor or host down with it.

```bash
# Start (idempotent)
scripts/surreal-start.sh

# Stop (either launcher)
scripts/surreal-start.sh --stop

# Follow logs (also written to data/surreal.log)
journalctl --user -u urban-surreal -f

# Override the hard memory cap (default 12G)
URBAN_SURREAL_MEM_MAX=8G scripts/surreal-start.sh
```

### Never launch from a worktree

`scripts/surreal-start.sh` derives the systemd service's `WorkingDirectory` from its own location,
and the store path it passes is **relative** (`rocksdb://data/surreal-rocks.db`). Run it from a git
worktree and SurrealDB starts *in that worktree*, where `data/` does not exist — `data/` is
gitignored, so it never arrives with a checkout or a `git worktree add`. RocksDB creates it. The
result is a perfectly healthy, perfectly empty instance: `/health` returns 200, `connect_db()`
succeeds, `SCHEMA_DEF` applies, every DB-gated test passes, and `INFO FOR ROOT` lists no
`urban_integrity` namespace at all.

Nothing in the workspace fails on this. On 2026-09-03 it cost two invalid live-corpus verifications
and one wrong documentation edit before the *duration* of the gated CLI suite gave it away (see
[Measured gate status](#measured-gate-status)). It is worth knowing specifically when running
parallel agents in worktrees, which is the situation that produces it.

**Always launch from the repo root**, and assert the target before trusting any live-corpus result:

```bash
systemctl --user show urban-surreal -p WorkingDirectory   # must print the repo root
```

If it does not, `systemctl --user stop urban-surreal` and re-run `scripts/surreal-start.sh` **from
the repo root** — the launcher no-ops against an instance that still looks healthy, so the stop is
required. A worktree can share the repo-root instance perfectly well; it just must not start its
own.

### Recovering a wedged engine

A long, heavy write pass can leave SurrealDB *alive but unresponsive* — `/health` answers, memory
sits high (~10 GB), and every query hangs. A redundant full PPD re-pass did exactly this: hours of
`Transaction conflict: Resource busy` from thrashed background tasks, ~2% CPU, no forward progress.
The engine is not going to recover on its own, and **re-running the launcher will not help** —
`scripts/surreal-start.sh` treats a healthy-looking instance as already up and no-ops. Stop it
first:

```bash
# 1. Kill only the client first (the `enrich` / `project` / `backfill` process).
#    If queries then recover, you are done — do not restart the DB.

# 2. If queries still hang while /health is up, the engine itself is wedged:
systemctl --user stop urban-surreal
scripts/surreal-start.sh
```

The restart is crash-safe — RocksDB replays its own WAL and resident memory drops back to a few
hundred MB. Never work around a wedged instance by launching a second `surreal start` by hand: that
escapes both the RocksDB pin and the cgroup cap (see the box above).

Default credentials are `root` / `root`. The launcher hardcodes `--user root --pass root` (`scripts/surreal-start.sh`) — there is no env indirection on the server side. Client-side credentials are read from `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` (double underscore; the single-underscore form in `.env.example` is never read — see [Environment Variables and `.env` File](#environment-variables-and-env-file)).

---

## Test Namespace Hygiene

Every DB-gated test runs in its own throwaway SurrealDB namespace, so tests never see each other's
rows or production data. That namespace must come from **`urban-integrity-test-support`**
(`crates/test-support`) — never from a hand-rolled `format!("test_{uuid}")`.

| API | Use when |
|-----|----------|
| `TestNs::connect(db_name)` | The test drives SurrealDB through a `Surreal<Any>` handle. Returns a connected handle with `SCHEMA_DEF` already applied; derefs to `Surreal<Any>`, so `db.query(...)` and `&db` work unchanged at call sites that previously held a bare handle. `.arc()` yields `Arc<Surreal<Any>>`; `.ns()` yields the namespace name. |
| `NsGuard::new()` / `NsGuard::with_prefix("test…")` | The test drives the DB through something that only needs the *name* — a spawned CLI subprocess, a raw HTTP config. |

Both issue `REMOVE NAMESPACE` on drop, so **the guard must stay bound for the whole test**. Binding
it to `_` drops it immediately and takes the namespace with it:

```rust
let db = TestNs::connect("integrity").await;   // namespace created
db.query("SELECT * FROM entity").await?;       // via Deref
// namespace removed here, when `db` drops

let (db, _ns) = /* … */;                       // `_ns`, never `_`
```

**Why this exists.** Before the crate, each test file rolled its own `test_{uuid}` and nothing ever
removed it. One `URBAN_DB_TESTS=1 cargo test --workspace` leaked a few hundred namespaces per run,
and **2,889 had accumulated in the local store by 2026-07-31**.

`Drop` runs on normal return and on unwinding panics, including failed assertions. It cannot run
when the test binary is SIGKILLed or aborts, so a crashed run can still strand a namespace. Sweep
those with:

```bash
scripts/surreal-drop-test-ns.sh          # dry run (default) — lists what would go
scripts/surreal-drop-test-ns.sh --apply  # actually remove them
```

The script only ever touches namespaces whose name starts with `test` — the prefix `NsGuard`
enforces — so `urban_integrity`, `urban` and `main` can never match. Removals are batched 50
`REMOVE NAMESPACE IF EXISTS` statements per request. Endpoint and credentials come from
`URBAN_TEST_DB_URL`, `URBAN__SURREAL__USERNAME`, `URBAN__SURREAL__PASSWORD`.

`db_tests_enabled()` in the same crate is the gate the tests use: `URBAN_DB_TESTS=1` **and** a TCP
pre-flight confirming SurrealDB is actually listening. The pre-flight makes a missing DB skip the
test rather than panic halfway through it — which is also why a green `cargo test --workspace`
tells you nothing about whether the DB tests ran.

---

## Proving a Test Can Fail

A test that has never been observed to fail is not evidence for the thing it names. This workspace
has shipped several tests that could not fail for the reason they claimed, and every one of them
was green the whole time.

**Verify a falsifiability guarantee by mutation, not by re-reading the assertion.** Break the
production code the test is supposed to catch, watch the test go red, then restore it. Re-reading
the assertion only re-runs the reasoning that produced the defect in the first place.

The clearest recent case comes from a security audit on 2026-09-03.
`every_verdict_variant_renders_in_detail_groups_or_the_skipped_section`
(`crates/probe/src/report/mod.rs`) exists to guarantee that every `Verdict` variant
reaches the reader's prose, so a future eighth variant cannot be added and rendered nowhere. It
asserted `report.contains(&entry.finding.probe)` — containment anywhere in the whole report. But
the report's ledger table prints **every** probe's name regardless of verdict, so that assertion
was satisfied for every entry no matter what the prose sections did. Deleting the `skipped::render`
call, or dropping a row from `DETAIL_GROUPS`, left it green. It now asserts on each prose home's
own marker — `### {probe}` for a detail-group entry, `- {probe} — ` for a skip bullet — and
requires **exactly one**, which also catches a variant acquiring a second home. Both mutations were
then actually run: each fails the new assertion, and each passed the old one. The repair was
test-only; no production code changed.

Three shapes to watch for, all of which have appeared here:

- **Containment against a superset.** Asserting a string appears *somewhere* in a large artifact
  when something unrelated in that artifact already guarantees it.
- **A negative assertion issued through a broken read.** "Are there zero matching rows?" asked via
  a row-losing query shape **passes when the bug bites** — the defect makes the test that would
  catch it succeed. Every instance found on 2026-08-21 lived in that blind spot; see [The guard,
  and where it stops](#the-guard-and-where-it-stops).
- **A count that cannot notice a justified addition.** `every_allowlist_entry_is_justified_and_live`
  passes happily against a newly added, properly justified `ALLOWLIST` entry, which is exactly why
  `allowlist_gained_no_entries_this_phase` pins the count separately.

The corollary for review: when a summary claims a guarantee is enforced by a test, the question is
whether anyone has seen that test red. The probe-port work recorded transcripts of three tests
deliberately broken and observed to fail before being accepted as evidence — that is the bar.

---

## Query Shapes That Lose Rows Silently

Three query shapes under-report **successfully** against SurrealDB 3.x: the query returns `Ok`,
with fewer rows than exist, and no caller has any signal that anything was missed.
[ARCHITECTURE.md § SurrealDB 3.x Compatibility Workarounds](ARCHITECTURE.md#surrealdb-3x-compatibility-workarounds)
documents the mechanism and the full `scan` API surface; what follows is the rule you have to hold
while writing code.

**Rule 1 — an indexed equality paired with a predicate on an unindexed field.** Pairing
`WHERE kind = 'NodeAdded'` with a `CONTAINS` on the unindexed `payload.*` makes the planner take
the index path and return a truncated, run-to-run non-deterministic row set. `count()` on the
identical predicate stays correct, which is what makes it so hard to spot. The rule is not specific
to `kind`: it applies to any indexed field on any watched table.

**Rule 2 — a large bare `LIMIT` with no paging.** A single large `LIMIT` returns well below the
requested count when the rows are big, so the query samples a fraction of the table and looks like
it read everything.

**Rule 3 — an ordered read with a `LIMIT` ceiling and no `START`.**
`ORDER BY confidence DESC LIMIT 100` is not a truncation bug, it is a *bias* bug: the read is
complete for the top of the ordering and empty for everything past it, at any magnitude. `links
list` and `flips list` both shipped this shape and could not see a candidate past their own
ceiling.

**Adding an index is not the fix for Rule 1.** Index *availability* is what triggers the misplan —
the index on `kind` is the reason the planner takes the bad path at all. The fix is to keep the
indexed field out of the `WHERE` clause, filter in Rust, and advance `START` by the number of rows
*actually received*.

What these shapes have already cost:

- the CH backfill ran against **18%** of its input set (6,033 rows where `count()` said 33,046)
- `ch_psc` enriched **0.27%** of its candidate companies (39 rows where `count()` said 14,198) and
  recorded `candidates_found: 0` while reporting success
- the `officer` and `psc` tables sat empty with no error anywhere in the logs

### Writing a safe scan

Route through the shared helpers rather than hand-rolling the query. All of them live in
**`urban_integrity_core::persistence::scan`**, take `&Surreal<Any>`, page at `SCAN_PAGE_SIZE`
(20,000), and end the loop only on an **empty** page — a short page means truncation, not
end-of-data. A `SCAN_MAX_ROWS` cap (2,000,000) logs a truncation warning instead of looping
forever.

| Table | Helpers |
|-------|---------|
| `graph_events` | `scan_events_by_type`, `find_event_by_type`, `scan_events_by_any_type`, `scan_events_of_kinds_by_type`, `scan_events_by_kind(s)`, `scan_events_by_kinds_paged`, `scan_events_by_payload_contains`, `event_payload_contains`, `scan_events_by_type_paged`, `scan_weights_by_type`, `scan_entities_by_type` (per **event**), `scan_latest_entities_by_type` (per **entity**) |
| `identity_links` | `scan_identity_links`, `scan_identity_links_paged`, `identity_link_exists` |
| `land_flip_candidates` | `scan_land_flip_candidates`, `scan_land_flip_candidates_paged` |

`identity_link_exists` is deliberately its own function rather than a `limit` parameter on
`scan_identity_links`: it returns a `bool`, and one matching row fully answers "does at least one
exist", so an existence check can never be mistaken for — or reported as — a count.

For the multi-hour backfill paths, **`crates/ingestor/src/backfill/mod.rs::scan_events_by_type`**
gives the same guarantee behind that crate's re-authenticating session wrapper. It is
`pub(crate)`, so out-of-crate callers use the `core::persistence::scan` helpers above.

### The guard, and where it stops

`crates/core/tests/query_shape_guard.rs` is a **source-level** test, deliberately: all three
defects under-report successfully, so there is no runtime assertion that can catch them and the
only reliable defence is making the shape impossible to write. It lexes every `crates/**/*.rs` the
way rustc does — comments, char literals and raw strings consumed as units — extracts string
literals that look like SQL against a watched table, and fails the build on any of the three rules.

**Test files are in scope.** They used to be skipped wholesale, on the reasoning that a regression
test needs to be able to write the broken shape. That is true of *demonstration* queries and false
of *assertion* queries: a negative test asking "are there zero matching rows?" through the
row-losing shape **passes when the bug bites**, so the defect makes the test that would catch it
succeed. Every instance found on 2026-08-21 — three negative tests in `property_accumulation.rs`,
one in `evasion_detector.rs` — lived in that blind spot. Only the guard file itself is exempt,
because its own unit tests hold the broken shapes as fixtures.

`WATCHED_TABLES` is the single list both the literal extractor and the rule matcher drive off, so a
table can only be added in one place. Each entry carries its own Rule-2 `LIMIT` threshold and the
indexed fields Rule 1 checks:

| Table | Rule-2 threshold | Why |
|-------|-----------------|-----|
| `graph_events` | 1,000 | Sized for `weight_json` response-size risk — a single large response can truncate well under a full-table read |
| `identity_links` | 1 | Any unpaged sample is suspect (PRB-04/GRD-03/GRD-09) |
| `enrichment_runs` | 1 | Schemaless with no indexes at all, so Rule 1 has no index path to misuse — its indexed-field list is deliberately empty |
| `oc_quota` | 1 | No real code issues a bare-`LIMIT` `SELECT` here today |
| `land_flip_candidates` | 1 | Any unpaged sample is suspect. `flips.rs`'s old `ORDER BY created_at DESC LIMIT 100` was fixed rather than threshold-dodged; the pre-fix shape survives only as an allowlisted A/B baseline |
| `ppd_transactions` | 1,000 | **Deliberately not 1** — a review assessed `land_flip.rs`'s `LIMIT 200` gate-1 join and closed it: the join is scoped to one exact `(postcode_norm, paon_norm, street_norm)` address triple, and 200 PPD transactions against a single address is not a reachable state. The threshold is that assessment's closure, not unfinished work |
| `land_flip_runs` | 1 | Single-record marker table with no `DEFINE INDEX` — the empty indexed-field list is deliberate, same shape as `enrichment_runs` |
| `land_flip_gate1_survivors` | 1 | Any unpaged sample is suspect; mirrors `land_flip_candidates`'s indexed-field list |
| `analyses` | 2 | **Deliberately not 1** — `registry.rs`'s `WHERE db_name = $db_name LIMIT 1` is a genuine unique-key point lookup (`db_name` carries a UNIQUE index), so at most one row can ever match |

Do not "fix" the `ppd_transactions` or `analyses` thresholds back down to 1 without reading the
comment above each of them in the source — both were chosen to avoid flagging a genuinely safe
read, and both say so in full.

> **The guard is narrower than the rule.** `broken_rule()` only reports Rule 1 when the query also
> contains `CONTAINS`. A site pairing an indexed equality with an **equality on an unindexed
> field** is the same defect and passes the guard clean. Treat it as a backstop for the shapes that
> have already bitten, not as proof that a query is safe.

Two further boundaries worth knowing: `count(` **anywhere** in the query exempts it, and Rule 1 is
checked before Rules 3 and 2 so a magnitude-independent diagnosis always wins over a
magnitude-dependent one.

> **The `count(` exemption was broader than its justification — narrowed 2026-09-01.** `count()` on
> a predicate does stay correct under the planner bug, which is why the exemption exists. But
> `broken_rule()` used to return `None` on the *presence of the substring*, before any rule was
> evaluated, so a query whose outer projection is a `count()` over a row-materialising **inner**
> `SELECT` was exempted while still containing the Rule-1 shape. That was a real finding against
> `persistence::oracle` (found in code review), not a
> hypothetical — it is how the oracle's two dedup primitives came to emit a verbatim Rule-1 shape
> invisibly. `broken_rule` now exempts `count(` only when the query contains no nested `SELECT`.
> Wrapping a read in `count(` is still not a way to make it safe.

The lexer is not incidental complexity. Naive quote-pairing inverts phase on a single `'"'` char
literal — `s.trim_matches('"')` appears throughout this workspace — after which every real query
reads as a gap. That defect shipped with the guard and hid two violations in
`property_accumulation.rs` and three in `queries.rs` behind a green test with an empty allowlist.

**`ALLOWLIST` holds eight entries, and not one of them is an ordinary read.** One is the
planner-bug reproduction in `surreal_integration.rs`. Five were added with the ordered-read rule: `scan.rs`'s
`identity_link_exists`, which returns a `bool` and has no coherent "fix" that returns more rows
without changing what the function means, plus four pre-fix A/B baselines — a test issues the old
ceiling-limited shape verbatim, asserts it returns exactly the ceiling and omits a seeded marker
row, then asserts the paged replacement surfaces it. Rewriting those queries to the fixed form
would delete the evidence that the fix changes real behaviour. The last two were added on
2026-09-01 for `persistence::oracle`'s two dedup primitives. Each entry is
`(path, query_fragment, reason)`, keyed by a distinctive query fragment rather than a line number,
and two tests police the list itself: `every_allowlist_entry_is_justified_and_live` rejects an
empty reason or a fragment that no longer matches anything, and
`allowlist_gained_no_entries_this_phase` pins `ALLOWLIST.len()` against `ALLOWLIST_BASELINE`
(**8**). The second exists because the first passes happily against a newly added, properly
justified entry. A genuinely necessary new exemption is an explicit operator decision recorded in a
plan — never a silent widening to make a test pass, and never an unexplained one, because "not
fixed" must not be indistinguishable from "not noticed".

> **The 6 → 8 raise was not a widening.** The two `oracle.rs` entries became *visible* only because
> `broken_rule`'s `count(` exemption was deliberately **narrowed** in the same change to exclude
> queries containing a nested `SELECT` (see the box above). The net effect is that two previously
> invisible Rule-1 shapes are now recorded, justified and re-checkable rather than silently waved
> through. Each of the two reasons carries its live cross-check evidence — a third, unrelated shape
> (`array::len(array::distinct(...))`) agreeing exactly on `an_anglesey_aebaa785`, 2026-09-01 — and
> states explicitly that the agreement is a **sample, not a proof**, because the planner bug is
> run-to-run non-deterministic. Re-run that cross-check before relying on the oracle against a new
> corpus, a new SurrealDB version, or a materially larger graph. Note the failure asymmetry the
> entries record: if the oracle ever under-counts, it presents as "the probe over-counts", and the
> recorded precedent is that the *probe* gets changed — so a correct probe could be "fixed" to
> match a truncated oracle.

### Reads too large to decode

The three shapes above lose rows *silently*. There is a fourth limit at the other end of the same
problem, and it fails **loudly**: the SurrealDB 3.0.x server / `3.1.0-beta.2` SDK pairing cannot
wire-decode a large response at all. Roughly **55k rows decode; ~443k do not**. Past the ceiling
the SDK returns:

```
The server returned an unexpected response: Failed to deserialize flatbuffers payload:
Failed to decode fb value
```

Loud is not the same as harmless. On 2026-08-22 `load_icij_name_index` issued a single unpaged
`SELECT node_id, name FROM icij_offshore_entity LIMIT 5000000` against a table holding **814,315**
rows. It aborted the OCOD re-emit *after* `purge-ocod-owners` had already deleted the 127,407
superseded events — the analysis was left with no OCOD ownership until the loader was paged and
the re-emit re-run. It had never bitten before only because the ICIJ adapter wrote 0 rows for its
entire life (Standing Rule 1), so the loader had only ever read an empty table.

Two independent reasons to page, then. Route reads through
`urban_integrity_core::persistence::scan` (`SCAN_PAGE_SIZE` 20,000) rather than hand-rolling a
`LIMIT`. When a call site genuinely cannot use the shared helpers, two details are easy to get
wrong:

- **`ORDER BY id` needs `id` in the projection.** SurrealDB 3.x rejects an `ORDER BY` on a field
  that is not selected. Ordering is what keeps `LIMIT`/`START` paging stable so no row is skipped
  or repeated.
- **Terminate on an *empty* page, not a short one.** This is the guarantee
  `core::persistence::scan` documents and enforces, and it is the whole point of Rule 2 above: a
  short page means the response was truncated, not that the table is exhausted, so `START` must
  advance by the number of rows *actually received*.

  > This rule is easy to break by accident. `load_icij_name_index_paged` in
  > [`crates/enricher/src/adapters/land_registry_ocod.rs`](../crates/enricher/src/adapters/land_registry_ocod.rs)
  > shipped on 2026-08-22 breaking on `returned < page_size` while its own doc comment claimed it
  > was "the same loop shape `core::persistence::scan` uses" — it was not, and the mismatch was
  > caught by a documentation verification pass the next day rather than by a test, since the
  > bounded page it requested sat far below the truncation threshold. It now terminates on an
  > empty page with a `ICIJ_INDEX_MAX_ROWS` safety cap, matching `scan`. When in doubt, copy
  > `scan` rather than re-deriving the loop.

---

## Two-Path Verification (the population oracle)

`crates/core/src/persistence/scan` is the load-bearing read path for `graph_events` — nearly every
enricher adapter, every probe and most CLI commands go through it. That is exactly what makes a
defect in it invisible: every caller is wrong in the same way, and they all agree with each other.
A second, deliberately independent counting path exists,
[`crates/core/src/persistence/oracle.rs`](../crates/core/src/persistence/oracle.rs), so a defect in
either one surfaces as a **disagreement** rather than as two modules quietly agreeing on a wrong
number.

### ORC-06: `oracle.rs` must never call `persistence::scan`

The oracle is built from raw `db.query()` / `count()` / `GROUP BY` SurrealQL only. It contains zero
imports from, and zero calls into, `persistence::scan`. This is requirement **ORC-06**, and it is
enforced structurally rather than by convention:
[`crates/core/tests/oracle_independence_guard.rs`](../crates/core/tests/oracle_independence_guard.rs)
fails the build if `oracle.rs` gains a `scan::` call site or a `persistence::scan` mention **in
code**. It reuses `query_shape_guard.rs`'s byte-classification lexer, inverted — comments, char
literals and string literals are stripped first, and only what remains is matched — so the module's
own doc comment explaining the rule cannot trip its own guard. Three tests hold it up: the
invariant itself, a positive fixture proving the matcher fires on a real `scan::` call, and a
negative fixture proving it does not fire on a doc comment or on raw SurrealQL.

**Do not "helpfully" deduplicate the two implementations.** The duplication *is* the mechanism.
Merging them deletes the cross-check and leaves you with one implementation that cannot be wrong in
a detectable way. The same instruction is written on `scan::latest_entity_weights`, which also
tells you not to unify it with `export_graphml.rs`'s two copies of the same "greatest `timestamp`
wins" rule.

This is not a hypothetical concern. On 2026-08-27 `crates/cli/src/commands/export_graphml.rs`
independently derived the correct Contract population — **39,860** ward-reachable Contracts — while
three probes reading through `scan::scan_entities_by_type` reported **99,599**. Both numbers sat on
disk for four days because nothing compared them. A shared implementation would have been wrong in
both places and agreed with itself.

### Counting events versus counting entities

`graph_events` is an append-only log: re-ingesting an entity emits another `NodeAdded` row for the
same `entity_id`. Two scan helpers differ on exactly this, and picking the wrong one is the defect
class the oracle exists to close.

| Helper | Returns | Use it for |
|--------|---------|------------|
| `scan::scan_entities_by_type` | one entry per `NodeAdded` **event** | only when you genuinely want every event |
| `scan::scan_latest_entities_by_type` | one entry per **entity** (greatest `timestamp` wins) | any figure reported as a `population` |
| `oracle::count_distinct_entities` | a server-side `GROUP BY entity_id` count | the independent cross-check of the above |

`.len()` on the first is an **event count**. Reporting it as a population is defect **ADJ-01**, and
it has bitten twice:

- `probe/identity_integrity.rs` read 105,613 events for 45,874 distinct Contracts on
  `an_anglesey_aebaa785` (2026-08-31) — a 2.3x inflation that produced a false `Critical`
  "coverage REGRESSED" alarm.
- `probe/geographic_concentration.rs` carried the same defect into a threshold. Measured against
  `an_anglesey_aebaa785` on 2026-09-01: **102,897** `Location` `NodeAdded` events over **61,756**
  distinct Locations, of which **22,839** carried two or more events. Each of those crossed the
  "two or more applications on one street" threshold on its own duplicates alone. Switching to
  `scan_latest_entities_by_type` moved the probe's `matches` figure from **23,322** to **1,697**,
  exactly matching the Python reference implementation. Fixed in `c8fb0c7`.

Two details worth keeping straight when reading those numbers:

- 23,322 and 1,697 are **`matches`** — streets carrying two or more applications — not
  populations. Earlier phase docs conflated the two.
- `street_key` normalisation was the lead a differential run flagged, and it was **exonerated**: `street_key`
  and `postcode_sector` are verbatim ports of the Python functions, with parity tests asserting it.
  The scan was the cause.

`probe/temporal_burst.rs` is the one documented exception: it dedups on its own, because it needs
the **earliest** event per Contract and the raw row's `timestamp`, which
`scan_latest_entities_by_type`'s signature drops. That is a stated exception with a comment
explaining it, not a licence to hand-roll a dedup.

### The oracle API

Four functions, all `async fn(db: &Surreal<Any>, ...)`, all binding every
externally supplied value as a query parameter; the three counters return `anyhow::Result<u64>`:

| Function | Counts |
|----------|--------|
| `count_events(db, kind, markers)` | `graph_events` rows matching `kind`, ANDed with zero or more `payload.weight_json CONTAINS` markers — one row per **event** |
| `count_distinct_entities(db, kind, type_marker, entity_id_prefix)` | distinct `payload.entity_id`, via `GROUP BY` before the outer `count()` — the dedup-by-entity primitive |
| `count_distinct_edge_field(db, type_marker, field)` | distinct `payload.src_id` or `payload.dst_id` (`EdgeField::Src` / `EdgeField::Dst`) for `EdgeAdded` rows carrying `type_marker`; groups by one side only and never inspects the other |
| `max_timestamp_for_prefix(db, kind, entity_id_prefix)` | not a count: the maximum `timestamp` among `kind` rows whose `payload.entity_id` starts with the prefix, returned as `anyhow::Result<Option<u64>>` |

Correctness is proven by
[`crates/core/tests/oracle_shapes.rs`](../crates/core/tests/oracle_shapes.rs) — seven DB-gated
tests, including one that cross-checks `count_distinct_entities`'s `GROUP BY` result against
`scan_latest_entities_by_type`'s `.len()` on a controlled fixture. That cross-check is the whole
point: two mechanisms (server-side grouping versus Rust-side timestamp folding) reaching the same
number.

> **Query-shape safety: the immunity claim was withdrawn, and two queries are exempted by explicit
> assessment.** `oracle.rs` used to assert the module was immune to the SurrealDB index-planner
> truncation bug because it issues only `count()`/`GROUP BY` and "never a row-returning `SELECT`".
> That was false and has been withdrawn (finding **CR-01**, fixed 2026-09-01). Accurate position:
> `count_events` is a bare server-side aggregate and genuinely inside the proven-safe family;
> `count_distinct_entities` and `count_distinct_edge_field` each wrap a row-materialising inner
> `SELECT` pairing indexed `kind` equality with an unindexed `payload.weight_json CONTAINS`.
> `query_shape_guard.rs`'s `count(` exemption has been narrowed to exclude nested selects, so both
> are now flagged and carry `ALLOWLIST` entries recording why they are accepted
> (`ALLOWLIST_BASELINE` 6 → 8, an explicit recorded decision). **That acceptance rests on a live
> third-shape cross-check that is a sample, not a proof** — the planner bug is run-to-run
> non-deterministic. Re-run it before trusting the oracle on a new corpus, a new SurrealDB version,
> or a materially larger graph.

### `probe-oracle` — the comparison and coverage command

`crates/cli/src/commands/probe_oracle.rs` runs the real battery via `probe::run::execute`,
independently recomputes each checkable probe's population through `oracle::`, and prints two
tables:

- **Comparison** — `Probe | Declared | Oracle | Delta | Agrees`, with declared and oracle counts
  always on the *same* row. `declared` is `None` (rendered "not measured this run") when the probe
  returned `DataGap`/`Blocked`, so a probe that measured nothing can never register as a false
  disagreement against zero. `PROBE_DELTA` — the per-probe allowed delta table — is **empty
  today**: every checkable probe is expected to match exactly, and a non-zero entry must carry a
  non-empty justification, which `probe_delta_is_justified` enforces.
- **Coverage / exclusions** — `Probe | Category | Reason`, naming every probe the oracle *cannot*
  check together with a source-cited reason. Never a blanket "Tier-2" label (ORC-08).

Of the 22 registered probes, **10 are checked** (`precomputed-alerts`, `identity-integrity`,
`address-cluster-exact`, `geographic-concentration`, `ward-concentration`, `temporal-burst`,
`gazette-insolvency-signals`, `property-nodes`, `osint-new-node-types`, `phase20-council-registers`)
and **12 are excluded**, under four categories:

| `ExclusionCategory` | Meaning |
|---------------------|---------|
| `IdentityLinksSourced` | population comes from `identity_links`, not `graph_events` |
| `VerdictThreshold` | population is a hardcoded `Verdict::classify(1, 1, …)` threshold, not a real count |
| `SemanticMismatch` | population counts something structurally different from an entity/edge count (a term count, a scalar marker field) |
| `MarkerArithmetic` | population is arithmetic on scalar fields of a single marker record |

The `geographic-concentration` row is worth understanding before you touch either side of it: it is
the entry that **disagreed** live and exposed ADJ-01. It now agrees, and it stays exactly as it is
as the regression guard. Do not adjust either side to force agreement.

Two known limitations, neither of which should be papered over:

- **`--output` can pick up ANSI escape codes.** Table styling is applied when stdout is a TTY, so
  an artifact written from a terminal session is not necessarily clean plain text (finding WR-03,
  still open).
- **The oracle's two dedup primitives are allowlisted, not proven safe.** Their acceptance rests on
  a live third-shape cross-check that is a sample rather than a proof (finding CR-01, fixed
  2026-09-01 — see the query-shape note above). Re-run it for a new corpus or SurrealDB version.

```bash
# Print both tables to stdout
cargo run -p urban-integrity-cli -- probe-oracle

# Also write a rendering of both tables to disk (see the ANSI caveat above)
cargo run -p urban-integrity-cli -- --analysis <db_name> probe-oracle --output oracle.txt
```

The DB-gated tests are `crates/core/tests/oracle_shapes.rs` (correctness of the counting shapes)
and `crates/core/tests/oracle_independence_guard.rs` (the ORC-06 source guard, which needs no
database).

---

## Analysis Lifecycle (multi-analysis databases)

The CLI can operate on multiple SurrealDB databases ("analyses") within the same namespace — for example a per-LPA ward-set database cloned from a national baseline. The lifecycle machinery lives in `crates/core/src/analysis/naming.rs` (pure naming functions), `crates/cli/src/registry.rs` (`_catalog` registry access), `crates/cli/src/db.rs` (`resolve_analysis_db`), and `crates/cli/src/commands/analysis.rs` (the subcommands).

### Global `--analysis` selector

Every subcommand accepts a global `--analysis <db_name>` flag (omitting it behaves like `main`). It is bridged to the `URBAN__ANALYSIS` env var so every command's `AppConfig::load()` sees it. Only an absent flag or `main` connects directly to the `config.toml [surreal] database` with no registry access; **every other name — including the reserved `uk_national`** — is resolved through the `_catalog` registry and must exist there as a non-`retired`/`retiring` row.

```bash
# Run a report against a provisioned analysis
cargo run -p urban-integrity-cli -- --analysis an_black_country_2026_1a2b3c4d report --min-score 0.5

# Enrich a specific analysis DB
cargo run -p urban-integrity-cli -- --analysis an_foo_deadbeef enrich --source ppd
```

### Naming

Analysis database names are content-addressable: `an_<label_slug>_<hash8>`, where `<label_slug>` is the slugified label (`[a-z0-9_]`) and `<hash8>` is the first 8 hex chars of SHA-256 over the sorted, de-duplicated ward-code set (order- and duplicate-independent — the same ward set always hashes the same). Names are validated against `^[a-z][a-z0-9_]*$`; reserved names are refused as `create` targets. Each analysis gets its own checkpoint directory (`checkpoint_path_for`), created by `analysis create` and deleted by `analysis remove`. Note that this is **provisioning only** — nothing writes or reads checkpoints at runtime (`write_checkpoint`/`hydrate_from_checkpoint` have no call sites; `serve` rehydrates by full WAL replay), so the directory is currently an empty placeholder rather than an active isolation boundary.

### `analysis create` — provision a new analysis

```bash
# Provision empty
cargo run -p urban-integrity-cli -- analysis create \
  --label "Black Country 2026" --wards-file wards.txt

# Clone a baseline database (whole-DB via surreal export | import)
cargo run -p urban-integrity-cli -- analysis create \
  --label "Black Country 2026" --wards-file wards.txt --from uk_national \
  --boundary-vintage "ONS-2025"
```

Key behaviours:
- `--wards-file` is a file of ONS GSS ward codes, one per line; each code is shape-validated (one uppercase letter + 8 digits, e.g. `X05999001`).
- `--from <db>` clones a baseline database via `surreal export | import`. The clone is **one-directional and read-only on the source** — it never writes back to the baseline. It carries `graph_events` plus cursor tables so streaming can later resume against the clone. The `surreal` binary is located via `SURREAL_BIN`, then `~/.surrealdb/surreal`, then `surreal` on `PATH`.
- Creation is idempotent-guarded (an existing registry row for the same db name is refused), applies the schema, creates the per-analysis checkpoint dir, and flips the registry row to `active`. A failed clone is torn down best-effort.
- `--boundary-vintage` records the ONS boundary edition in the registry (default `"unknown"`).

### `analysis list` — inspect provisioned analyses

Lists registry rows, newest first, including each analysis's `graph_events` count (connecting applies the schema idempotently; a fresh DB reports 0).

### `analysis remove` — retire an analysis

```bash
# Dry-run (default): report what would be removed, DB untouched
cargo run -p urban-integrity-cli -- analysis remove an_foo_deadbeef

# Actually drop the database, delete its checkpoint dir, mark the registry row retired
cargo run -p urban-integrity-cli -- analysis remove an_foo_deadbeef --apply

# Retire an analysis still inside the Phoenix detection window (explicit intent)
cargo run -p urban-integrity-cli -- analysis remove an_foo_deadbeef --apply --decommission
```

`remove --apply` refuses analyses still inside the Phoenix detection window (measured from the registry `created_at`, not event timestamps — the WAL `timestamp` on `graph_events` is unreliable; the fallback window matches the detector default of 180 days) unless `--decommission` is passed. This prevents accidentally dropping evidence that is still within a live detection window.

### `_catalog` registry

Registry rows live in the `analyses` table of the `_catalog` database (same namespace). `connect_db()` resolves the `--analysis` value through this registry before connecting, so an unknown analysis name fails fast rather than silently creating an empty database.

An `AnalysisRow` carries `db_name`, `label`, `ward_codes`, `wardset_hash`, `schema_version`,
`checkpoint_path`, `status`, `created_at`, the optional baseline/boundary vintages, `retired_at`,
`notes`, and `coverage_baseline_json` — the last being the regression baseline the
`identity-integrity` probe compares planning-applicant provenance coverage against. `db_name`
carries a UNIQUE index, which is why the query-shape guard's threshold for the `analyses` table is
2 rather than 1: a `WHERE db_name = $db_name LIMIT 1` there is a genuine point lookup.

---

## Benchmarking

The project uses **Criterion.rs** for benchmarking. Run with:

```bash
# End-to-end Ingest-to-Alert latency (macro benchmark)
cargo bench -p urban-integrity-core --bench macro_bench

# Motif detector traversal speed (micro benchmark)
cargo bench -p urban-integrity-detector --bench motif_detectors

# PERF-03: 10k concurrent CSR readers (Criterion)
cargo bench -p urban-integrity-core --bench concurrent_readers

# Real-time P50/P99 latency reporting (example binary)
cargo run -p urban-integrity-core --example latency_monitor
```

Benchmarks run in `--release` profile. Core isolation via `taskset` reduces "noisy neighbour" variance:
```bash
taskset -c 2 cargo bench -p urban-integrity-detector --bench motif_detectors
```

### Performance Targets

| Metric | Target | Achieved |
|--------|--------|---------|
| Ingest-to-Alert P99 | < 500ms | ~3ms |
| 4-hop graph traversal | < 10ms | sub-1ms |
| 10k concurrent CSR reads P99 (PERF-03) | < 10ms | **2.5µs** |
| SurrealDB commit | < 100ms | AIMD self-tunes — target 50ms |

---

## Report Export

The `report` command supports four output formats via `--format`: `table` (default), `json`, `md`, and `pdf`.

```bash
# Markdown report scoped to a ward
cargo run -p urban-integrity-cli -- report --ward X05999001 --format md

# PDF report (requires pandoc; falls back to wkhtmltopdf)
cargo run -p urban-integrity-cli -- report --ward X05999001 --format pdf --output-file report.pdf

# Entity-scoped report (cross-ward, by company name or CH number)
cargo run -p urban-integrity-cli -- report --entity "Apex Build Ltd" --format md

# Append LLM narrative summary via Ollama (degrades gracefully if Ollama is unavailable)
cargo run -p urban-integrity-cli -- report --ward X05999001 --format md --summarize
```

Key behaviours:
- `--output-file` is only valid with `--format md` or `--format pdf`.
- For PDF output, the Markdown file is always written first and preserved even if PDF generation fails.
- PDF generation uses `pandoc` (primary) and falls back to `pandoc → html5 + wkhtmltopdf`; if both are absent an install-instructions error is returned.
- `--summarize` calls Ollama at `http://localhost:11434/v1` with the `gemma4` model. If Ollama is unavailable it emits a `WARN` log and continues without the narrative.
- Ingestor mappers now populate citation fields: `tender_ref` from the OCDS tender ID (`crates/ingestor/src/cf/mapper.rs`) and `application_ref` from the planning reference (`crates/ingestor/src/planning/mapper.rs`). These appear in the `## Source Data` section of Markdown/PDF reports.

---

## CQRS Projector

`crates/ingestor/src/projector.rs` implements the CQRS projection step: it reads the `graph_events` WAL table and projects its events into typed native SurrealDB tables (`entity`, `officer`, `location`, `contract`, `ward`, `political_actor` and their relation tables).

### Design

The module is split into pure helper functions and the `Projector` struct:

- `table_for_weight(weight)` — maps a `NodeWeight` variant to its SurrealDB table name
- `node_data(entity_id, weight)` — serialises a node weight to a `serde_json::Value` for upsert
- `edge_rel_table(weight)` — maps an `EdgeWeight` variant to its relation table name
- `edge_data(weight)` — serialises an edge weight to a `serde_json::Value` for `RELATE … CONTENT`

These functions carry no DB dependency and are covered by unit tests that run with plain `cargo test` (no SurrealDB required).

`Projector::run()` performs the full projection in five steps:

1. **Open the run marker** — `warn_if_previous_run_unfinished()` then `begin_run()`, before anything
   is destroyed, so a reader can always tell a partial projection from a finished one.
2. **Load and fold the `NodeAdded` stream** via raw HTTP `/sql` (the flatbuffers bypass, below),
   *then* wipe the 11 node tables (`entity`, `officer`, `location`, `contract`, `ward`,
   `political_actor` plus the OSINT tables `psc`, `charity`, `property`, `offshore_entity`,
   `disqualification`). Deduplication goes through the shared
   `persistence::queries::fold_latest_node_rows`, which keeps the highest-timestamp row so
   correction events win — and keeps the **correction** on an equal-timestamp tie. A hand-rolled
   `ts > entry.0` here kept the stale birth row instead, so the projected `entity` table disagreed
   with `entity --ch-number` for exactly the batch-backfill case (one clock read stamped across a
   whole batch).
3. **Upsert each folded node**, building the `entity_id → table` map the edge pass resolves against.
4. **Load the `EdgeAdded` stream** (verified against `SELECT count()`), *then* wipe the 12 relation
   tables (`directs`, `located_at`, `awarded_to`, `issued_by`, `represents` plus `controls`,
   `trustee_of`, `owns_property`, `offshore_link`, `donates_to`, `disqualified_from`,
   `declares_interest`).
5. **`RELATE` edges in batches of 200** statements per HTTP round-trip, then `finish_run()` stamps
   the completion marker and final counts.

> **Load-before-wipe is deliberate.** A single up-front `wipe_native_tables` destroyed both halves
> before a row was rebuilt, so any failure after it — a short load, a killed process, a dropped
> connection — left the database strictly worse than not running `project` at all (2026-08-11).
> Each half now survives until its own stream is in hand. That narrows the window; it does not
> close it. A process killed **during** the upsert or the `RELATE` loop still leaves a partial
> rebuild that looks exactly like a finished small graph, which is what the run marker is for.

### Edge-drop sanity gate

Projection surfaces edge loss instead of hiding it: skip counts are broken down by cause (`edges_skipped_unknown_src` / `edges_skipped_unknown_dst`), and if the edge-skip ratio reaches `MAX_EDGE_SKIP_RATIO` (**0.90**) the run **fails loud** with an error rather than reporting a broken projection as success — a ratio that high means the node map almost certainly failed to load (the replay/empty-graph class of bug). Ratios below the threshold are treated as lossy-but-usable (known historical debris from dangling edges sits well under it).

### Run marker: partial vs complete

**`project` is destructive-then-rebuild.** Each half of the native schema is wiped once its stream
has loaded, and re-derived from the WAL. That makes a run interrupted *after* a wipe
indistinguishable from a successful one over a smaller graph: the tables are internally consistent, nothing errors, and no
log line survives the process. On 2026-08-11 a run killed **12.6%** into the edge stream left five
relation tables empty and four partial — and it read as nine separate per-table bugs for four days.

The only thing that tells the two apart is the single record `projection_runs:current`
(`SCHEMALESS`, defined in `crates/schema/src/db.rs`):

| Field | Written by | Meaning |
|-------|-----------|---------|
| `started_at_ms` | `begin_run()` | Set at the top of every run, alongside `finished_at_ms: NONE` |
| `finished_at_ms` | `finish_run()` | Stamped **only** after the final batch flushes. Null while a run is in flight, and null *forever* for a run that died — SIGKILL included, which no in-process handler could catch |
| `nodes_upserted` / `edges_related` / `edges_skipped` | `finish_run()` | Final counts |

```bash
# Is the projected graph trustworthy?
curl -s -u root:root http://localhost:8000/sql \
     -H 'surreal-ns: urban_integrity' -H 'surreal-db: <db_name>' \
     -H 'Content-Type: application/json' -H 'Accept: application/json' \
     --data 'SELECT * FROM projection_runs:current' | jq '.[0].result[0]'
# finished_at_ms: null  → the last run died. The native tables are a partial rebuild.

# Per-table verdict (TRUNCATED / STALE / DROPPED / COMPLETE) against graph_events:
python3 scripts/projection_audit.py
```

Both markers go over the raw `/sql` HTTP path rather than the SDK handle, deliberately: the SDK's
websocket session expires on a long run. The 2026-08-15 rebuild related 498,888 of 498,986 edges
cleanly over **2 h 20 m**, then died stamping the completion marker with `401 Unauthorized`. A
completion marker that cannot be written at the end of a long run is worse than none — it reports
every slow success as a failure. `raw_sql` re-authenticates per request.

A new run WARNs if the previous one never stamped itself complete
(`warn_if_previous_run_unfinished`), and readers that depend on projected data check the field
themselves: `derive_districts_from_db` in `crates/enricher/src/adapters/land_registry_ppd.rs`
**refuses** to build its ingest allowlist from a partial projection and returns an empty set, because
a short allowlist discards source data permanently — a later `project` does not bring those rows
back, only a re-ingest does.

[PROJECTION.md](PROJECTION.md) states the model this section implements, including the trap that
makes partial projections so hard to diagnose: `graph_events.payload.weight_json` is an opaque JSON
**string**, so a SurrealQL path like `payload.weight.some_field` returns 0 rows rather than an
error. Read it before concluding that a projected table is missing data because ingestion failed.

**Operational consequence.** A full projection over the live corpus takes well over an hour (the
2026-08-15 run: 2 h 20 m for ~499k edges) and starves concurrent reads while it runs. Do not start
one you cannot let finish, do not start one while an analyst is querying, and after any run that did
not exit cleanly, re-run it before trusting `entity`, `export-graphml`, or any adapter that reads
the native tables.

### SurrealDB 3.x workaround

SurrealDB 3.0.x server + 3.1.x SDK have a flatbuffers codec mismatch: relation records returned by `INSERT INTO <rel_table>` encode typed `in`/`out` `RecordId` fields that the SDK cannot decode. `Projector` bypasses this by posting edge queries directly to the `/sql` HTTP endpoint as plain JSON via `reqwest`. `RawSqlConfig` holds the base URL, namespace, database, and optional Basic Auth credentials derived from `config.toml`. The node-event `SELECT` (`NodeAdded` rows) uses the same bypass; only `upsert_node` (the `UPSERT type::record(...)` write) still goes through the SDK, which handles node upserts correctly.

OSINT node variants (Psc, Charity, Property, OffshoreEntity) and OSINT relation variants (Controls, TrusteeOf, OwnsProperty, OffshoreLink, DonatesTo, DisqualifiedFrom) are fully serialised and project alongside the core variants.

### Running the projector

```bash
# Project WAL into native tables (wipes and re-projects everything).
# Destructive-then-rebuild: let it finish, or the tables are a partial rebuild
# that looks exactly like a complete one. Over an hour on the live corpus.
cargo run -p urban-integrity-cli -- project

# After schema changes in db.rs, re-verify projection output counts
cargo run -p urban-integrity-cli -- project
```

### Development workflow for projector.rs changes

- Pure function tests (`table_for_weight`, `node_data`, `edge_rel_table`, `edge_data`, `edge_skip_ratio`) — run with plain `cargo test -p urban-integrity-ingestor`; no SurrealDB needed.
- E2E projection behaviour requires SurrealDB: `URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor`
- After any schema change in `db.rs`, run `urban-integrity-cli project` and confirm the node/edge counts in the output match expectations.

---

## Python Forensic Scripts & Shell Helpers

The `scripts/` directory contains the Python forensic and repair scripts, eleven shell helpers
(`report.sh`, `enrich.sh`, `export_graphml.sh`, `export_parquet.sh`, `check.sh`, `status.sh`,
`rebuild-anglesey.sh`, `sidecar-start.sh`, `build-workbench.sh`, `toy-analysis.sh`,
`deploy-demo.sh`), six `check-*.sh` CI guards (`check-abbreviations.sh`,
`check-fixture-not-shipped.sh`, `check-no-analysis-literals.sh`, `check-shim-drift.sh`,
`check-verdict-deserialize.sh`, `check-wasm-boundary.sh`), three SurrealDB lifecycle scripts (`surreal-start.sh`, `surreal-guard.sh`,
`surreal-drop-test-ns.sh`), and a maintainer-specific `wards` file holding the reference analysis's 14 ward codes. The Python
scripts connect directly to SurrealDB (`http://localhost:8000`) with credentials `root/root` and to
the Companies House REST API (`api.companieshouse.gov.uk`) where noted. They paginate `graph_events`
to find nodes, and most support `--dry-run` to preview events without writing.

> **Target the right database.** `scripts/surreal_target.py` gives every Python script the same
> analysis selector the Rust CLI uses, with the same four-level precedence (documented in the
> `surreal_target.py` module docstring): `--analysis <db_name>`, then `URBAN__ANALYSIS`, then
> `URBAN__SURREAL__DATABASE` (a direct env override that still beats the file), then the
> `config.toml` `[surreal].database` default. There is deliberately no hardcoded fallback — if
> none of the four resolves, the script exits rather than guessing. Before it existed each script
> hardcoded
> `"surreal-db": "main"` — which predates the analysis lifecycle, so they read and wrote a database
> nothing else targeted. Import the shared resolver in any new script rather than hardcoding a
> database name. See [CONFIGURATION.md § Python Forensic Scripts
> Configuration](CONFIGURATION.md#python-forensic-scripts-configuration).

> **Call the shipped function, never a re-implementation.** Three stdin/stdout debug shims exist so
> out-of-process tooling can use the same key functions the engine does:
> `cargo run -p urban-integrity-core --bin normalise-name` (`normalise_name`, the key
> `planning_identity` joins on), `cargo run -p urban-integrity-ingestor --bin applicant-slug`
> (`applicant_slug`, the `director:{slug}` key), and
> `cargo run -p urban-integrity-enricher --bin gate1-address-key` (the land-flip gate-1 address key).
> The reason is concrete: two near-identical slug functions already exist in this codebase
> (`naming::slugify` joins with `_`, `applicant_slug` with `-`), and a Python third copy that picks
> the wrong one produces a disjoint keyspace that joins nothing. When the land-flip funnel harness
> re-derived gate 1 in Python and independently produced zero candidates, that agreement was
> recorded as cross-validation — it was nothing of the kind. Both sides shared the same wrong
> assumption, and the real defect was a parser bug. Two implementations agreeing is not independent
> evidence.
>
> **Three disjoint `Location` keyspaces already exist in the shipped Rust**, so "which key
> function?" is not a hypothetical question. *Locations:* `address::normalise_address`
> (`crates/ingestor/src/address/mod.rs`) builds `LOC:{POSTCODE}:{HOUSE_NUMBER}` from a validated,
> uppercased postcode plus the leading numeric token of the street — and it has exactly two
> non-test call sites, `crates/ingestor/src/ch/mapper.rs` and `crates/ingestor/src/cf/mapper.rs`.
> `planning::mapper::location_id_for` (`crates/ingestor/src/planning/mapper.rs`, also used by
> `backfill/planning.rs`) builds `location:{address}` — lowercased, spaces replaced by `-`.
> **The FCA mutuals adapter is a third variant, and that one is a live defect.**
> `crates/enricher/src/adapters/mutuals_register.rs` (the registered-address branch of `run`) never
> calls `normalise_address`; it hand-rolls
> `format!("LOC:{}:{}", pc.replace(' ', ""), addr.chars().take(40).collect::<String>().trim())`.
> Both segments diverge from the documented key: the postcode is space-stripped but never
> uppercased or validated, and the second segment is the first 40 characters of the free-text
> address rather than an extracted house number. For `1 High Street, EC1A 1BB` a company lands on
> `LOC:EC1A1BB:1` while a society lands on `LOC:EC1A1BB:1 High Street`. Nothing joins any of the
> three, so a planning address, a Companies House registered office and a registered society's
> address at the same postcode are three unrelated `Location` nodes — and societies never cluster
> with companies under `AddressCluster`. The comment immediately above that code asserts the
> opposite ("societies are visible to AddressCluster on the same terms as companies"); it records
> the intent, not the behaviour. *Slugs:* `naming::slugify` (`crates/core/src/analysis/naming.rs`)
> joins with `_` while `applicant_slug` (`crates/ingestor/src/planning/mapper.rs`) joins with `-`;
> new code that picks the wrong one silently creates a further keyspace rather than failing.

### Prerequisites

```bash
pip install requests  # needed by the 6 scripts that use it; no virtualenv needed
# The rest use stdlib urllib (analyst_probe.py, officer_merge_audit.py,
# ch_cessation_backfill.py, ...) and prune_isolated_nodes.py makes no network call at all.
```

### Run order

Run the scripts in this sequence when bootstrapping a new graph or enriching an existing one:

```
1. pa_ingest.py                            # councillors as PoliticalActor nodes
2. resolve_planning_applicants_to_ch.py    # give planning applicants a real CH identifier
3. ch_backfill.py --officers-for-ch-entities
4. fix_edge_directions.py
5. normalize_officer_keys.py               # or rekey_officers_to_director_slug.py — see below
6. pa_link_officers.py
7. analyst_probe.py
```

After steps 1–6, restart `urban-integrity-cli serve` so the coordinator replays the newly written `graph_events` before running the probe.

The officer re-key / audit / split trio (`rekey_officers_to_director_slug.py` →
`officer_merge_audit.py` → `split_false_merged_officers.py`) is a **repair chain**, not part of
bootstrapping. Run it in that order or not at all: the re-key fuses on name alone, the audit
adjudicates the fusions against Companies House, and the split undoes the ones the audit rejects.

### Script reference

#### `pa_ingest.py` — Ingest Sandwell councillors

Scrapes the Sandwell ModernGov member index (single public page, no auth) and writes `NodeAdded` events for each sitting councillor as `PoliticalActor` nodes keyed `PA:{name_slug}`. These seed the NepotismBridge detector, which BFS-starts from `PoliticalActor` nodes.

```bash
# Preview events without writing
python3 scripts/pa_ingest.py --dry-run

# Write councillors to SurrealDB
python3 scripts/pa_ingest.py
```

Flags: `--dry-run`

#### `ch_backfill.py` — Resolve applicants to Companies House entries

Two modes:

- **Default mode** — Finds `Entity` nodes in the graph whose names match company name patterns (Ltd, PLC, etc.) and searches the CH REST API for each. Writes canonical `GB-COH:{company_number}` entity nodes, officer nodes (`OFF:{company}-{name_slug}`), and `Directs` edges for active officers. Name matching is **exact-match-only** (2026-06-13): a CH company is accepted only on an exact normalised-name match, mirroring the Rust `match_company` in `crates/ingestor/src/backfill/ch.rs`. The former fuzzy SequenceMatcher path is gone; near misses are treated as no match.
- **`--officers-for-ch-entities` mode** — For every existing `GB-COH:` entity already in the graph, fetches active officers by company number (no name search). Use this for bulk backfill after initial ingestion.

```bash
# Default: resolve applicants by name
python3 scripts/ch_backfill.py --api-key YOUR_KEY --dry-run
python3 scripts/ch_backfill.py --api-key YOUR_KEY

# Officers-only: fetch officers for all existing GB-COH entities
python3 scripts/ch_backfill.py --officers-for-ch-entities --api-key YOUR_KEY --dry-run
python3 scripts/ch_backfill.py --officers-for-ch-entities --api-key YOUR_KEY
```

Flags: `--api-key KEY` (falls back to `URBAN__ENRICHER__CH__API_KEY` — the REST key; the streaming key `URBAN__INGEST__CH_API_KEY` 401s against the REST endpoints this script calls), `--dry-run`, `--officers-for-ch-entities`

The script rate-limits itself to ~109 requests/minute (0.55 s sleep between CH calls) to stay under the 600/5-min CH limit. A 429 response triggers an automatic 60-second backoff.

#### `fix_edge_directions.py` — Add forward edges for Phoenix traversal

Two fixes applied in sequence:

1. **Directs forward edges** — `ch_backfill.py` wrote `OFF:→GB-COH:` edges; Phoenix needs `GB-COH:→OFF:` (outgoing from the active company). Writes reverse edges with a `-fwd` event ID suffix.
2. **AwardedTo bridge** — `CF-PARTY:` entities with a known `company_number` get bridging `GB-COH:{company_number}→Contract` edges so Phoenix can reach contracts via the canonical CH entity ID.

```bash
python3 scripts/fix_edge_directions.py --dry-run
python3 scripts/fix_edge_directions.py
```

Flags: `--dry-run`

#### `normalize_officer_keys.py` — Create shared officer nodes for cross-company detection

`ch_backfill.py` keys officers as `OFF:{company}-{name_slug}`, making them company-scoped and preventing Phoenix detection across companies. This script:

1. Reads all company-scoped `OFF:` nodes from the graph.
2. Writes canonical name-only nodes: `OFF:{name_slug}`.
3. Writes bidirectional `Directs` edges: `GB-COH:{company}→OFF:{name_slug}` and `OFF:{name_slug}→GB-COH:{company}`.

The bidirectional edges enable Phoenix Tier B traversal: `seed(active) → OFF:{name} → GB-COH:{dissolved}`.

```bash
python3 scripts/normalize_officer_keys.py --dry-run
python3 scripts/normalize_officer_keys.py
```

Flags: `--dry-run`

#### `pa_link_officers.py` — Link councillors to their CH directorships

For each `PoliticalActor` node in the graph:

1. Searches the CH officer search API for the councillor's name and accepts **exact matches only** — order-independent token-set equality, because CH formats officer titles `SURNAME, Forename(s)` while councillor names are `Forename Surname`. No similarity score, no phonetics, no edit distance.
2. Fetches their active appointments from CH.
3. Writes a `GB-COH:` entity node for each appointment company and a `PA:→GB-COH:` `Directs` edge.

This gives the NepotismBridge BFS a 2-hop path: `PA:{slug} →Directs→ GB-COH:{company} →Directs→ OFF:{officer}`.

```bash
python3 scripts/pa_link_officers.py --api-key YOUR_KEY --dry-run
python3 scripts/pa_link_officers.py --api-key YOUR_KEY
```

Flags: `--api-key KEY` (falls back to `URBAN__ENRICHER__CH__API_KEY` — the REST key, not the streaming key), `--dry-run`

#### `analyst_probe.py` — Adaptive forensic battery

Runs an adaptive battery of 22 SurrealQL probes against the live `graph_events` table, testing for Phoenix loops, address clusters, and nepotism bridges. **All 22 are now ported to `urban-integrity-cli probe`**, which also writes `evidence-report.md` — the Rust battery, not this script, is the production path, and it fixes the verdict-vocabulary defect this script still carries (its `CLEAR` verdicts can be claims about a population it never measured). Each probe returns a verdict (`HIT`, `WEAK`, `DATA_GAP`, `CLEAR`, or `BLOCKED`). When identity attribution is found to be broken, the strategy pivots to geographic and temporal clustering, which does not depend on named applicants. Writes `evidence-report.md` in the project root by default.

**The script is deliberately retained as the differential oracle** (PRB-07, operator decision 2026-08-31). The Rust-vs-Python differential across all 22 probes produced three *bucket-(c)* findings — real disagreements nobody had documented — that only an independent implementation could have surfaced, including one pair where **both sides returned the same verdict** and disagreed only on the population behind it, so a verdict-only comparison would have called it agreement. Retention costs close to nothing: zero third-party dependencies (stdlib only; its `SurrealClient` speaks raw HTTP `/sql` via `urllib`), no `requirements.txt`, no venv, last verified against **Python 3.11.7**. The binding constraint on that retention: any disagreement a future re-run finds is **re-derived independently against `graph_events` / `identity_links` / the native tables — never resolved by making the Rust side match Python's output.** Python is an oracle, not ground truth.

Connection is read from `config.toml` `[surreal]` section with env overrides: `URBAN__SURREAL__WS_URL`, `URBAN__SURREAL__USERNAME`, `URBAN__SURREAL__PASSWORD`, `URBAN__SURREAL__NAMESPACE`, `URBAN__SURREAL__DATABASE`.

```bash
# Run probe and write evidence-report.md
python3 scripts/analyst_probe.py

# Probe only — no report file written
python3 scripts/analyst_probe.py --no-report
```

Flags: `--no-report`

#### `resolve_planning_applicants_to_ch.py` — Give planning applicants a real identifier

Planning applicants enter the graph as free text: a `director:{slug}` keyed on a name and nothing
else. Two measured consequences (2026-08-11):

- **False clusters.** 47.5% of the 1,431 `AddressCluster` alerts collapse to fewer than three real
  parties once name variants are merged — `mr-a-mrs-j-example` and `mr-and-mrs-j-example` at one
  address is one couple counted twice; `l-d-c-ltd`, `y-d-c-limited`, `ydc-ltd` and
  `york-design-construction-ltd` are one firm counted four times.
- **Blind spots.** ClwydAlyn Housing holds 52 Anglesey applications across 12 spellings, and no
  single node holds more than 32 — so no motif can see the developer whole.

The script resolves those applicant strings against Companies House and writes back a real
identifier. Exact-identifier resolution only; ambiguity resolves to no match.

#### `ch_cessation_backfill.py` — Widen Phoenix's failed-company coverage

Fetches real `date_of_cessation` values from CH REST. The dissolution dates already in the graph are
*correct* (40/40 sampled match Companies House exactly) — the defect is **coverage**. The CH
company-profile stream only emits companies that change, so the graph's entire dissolved corpus was
one day wide: 1,965 entities all ceasing 2026-07-28, a national bulk-dissolution batch of 14,974
companies (against 103 and 161 on the adjacent days). A 150-company control sample of entities the
graph calls Active found 7.5% dissolved at CH. Phoenix windows on `|award_date - dissolved_dt| <= 180`,
so a company the graph wrongly believes is Active can never be the failed counterparty in a motif.

#### `cf_backfill_dates.py` — Backfill `date_published` on CF contracts

`cf/mapper.rs` originally read only `tender.datePublished`, a Contracts Finder extension present on
~17% of releases, and ignored `release.date`, which OCDS makes mandatory and which is present on
100%. That left 4,929 of 5,552 CF contracts undated, so Phoenix's award-date window could not be
evaluated for any of their candidate motifs. The mapper is fixed but **forward-only**:
`graph_events` stores the *mapped* weight, not the raw OCDS, so existing rows cannot be repaired by
re-projecting — hence this script.

Do **not** try to fix it with a re-ingest instead. `serve`'s CF poller takes roughly the 700 most
recent notices then advances a cursor, so it would never revisit these; seeding it from an early
date would walk the national firehose forward and turn a ward-scoped analysis into a national one.

#### `rekey_officers_to_director_slug.py` — One human, one node

Re-keys officer nodes from the per-company `OFF:{company}-{NAME}` form to the cross-company
`director:{slug}` form. `ch_backfill.py` wrote one officer node *per company*, so a person directing
five companies became five unconnected nodes — and Phoenix's motif (seed Entity → Officer →
dissolved Entity) requires one officer node shared by two companies. That is why 0 of 1,898
dissolved companies had a usable edge.

Scope discipline: this fixes Phoenix and nothing else. It yields no land-flip gate-3 benefit — zero
re-keyed officer slugs match any planning applicant slug.

> **`director:{slug}` is a repair target, not an enforced invariant — the producers disagree.**
> The project's load-bearing rule is that one human is one node across LPAs, keyed
> `director:{slug}`. `crates/ingestor/src/planning/mapper.rs` does exactly that
> (`director:{applicant_slug}`, lowercase, `-`-joined). The Companies House ingest path does not:
> `map_officer` in `crates/ingestor/src/ch/mapper.rs` — reached from the `"officer-appointment"`
> arm of `map_ch_event` — emits `OFF:{resource_id}-{NAME_SLUG}`, company-scoped, with the name
> **uppercased and `_`-joined**. The enricher never trusts either stored key: `ch_psc`'s
> `match_officer` probes `OFF:{company_number}-{slug}` then `director:{slug}`, and
> `ch_disqualified`'s `match_disqualified_officer` probes `director:{slug}` only — both
> re-deriving the candidate key from each Officer node's stored `name` via `normalise_name`,
> which is how they match nodes whose literal key is neither form. That is why this script exists
> at all. Do not read a node's key as evidence of which producer wrote it, and do not assume a
> new emitter will inherit the rule.

#### `officer_merge_audit.py` — Adjudicate the name-only merges

The re-key above fuses on **name alone**, because `graph_events` carries no date of birth; it
recorded 531 fusions it could not adjudicate. This script resolves all of them against Companies
House REST, which publishes the two things the graph lacks: a per-person `officer_id` and a
`{month, year}` date of birth. Verdict on the live corpus: 437 defensible, 94 not.

Until this runs, every Phoenix output that rests on a fused officer node is unvalidated.

#### `split_false_merged_officers.py` — Undo the wrong fusions

Splits the officer nodes the audit rejected — 77 of them carrying two or more **distinct dates of
birth**, i.e. demonstrably different humans. Run only after `officer_merge_audit.py`.

#### `land_flip_funnel.py` / `land_flip_pairable.py` — Measure the land-flip funnel

`land_flip_funnel.py` walks the funnel gate by gate; `land_flip_pairable.py` counts the
planning/PPD pairs that are structurally matchable at all, which bounds what any gate could
possibly return. The funnel harness calls the shipped Rust gate-1 shim rather than re-deriving the
address key in Python — see the warning at the top of this section for why that matters.

#### `prune_isolated_nodes.py` — Strip degree-zero nodes from a GraphML export

`export_graphml.sh` dumps the whole graph, including every node no edge touches: placeholder
entities, unlinked property records, officers whose only edge lives in a different analysis. Those
dominate the node count in a layout tool and push large exports past what most viewers will open.

Streams in two passes and never builds a DOM — exports run to hundreds of megabytes (252 MB /
~872k elements at time of writing). Pass 1 reads only `<edge>` elements and records referenced node
ids; pass 2 re-reads and copies through, dropping unreferenced `<node>` elements and freeing each
as it goes. Peak memory is the id set, not the document.

#### `sidecar-start.sh` — Launch the Scrapling planning sidecar

Runs `scrapling-sidecar/sidecar.py` as a transient user systemd service (`urban-sidecar`) on
`:8765`, detached from the calling terminal, falling back to `nohup` if `systemd-run --user` is
unavailable. Idempotent — a healthy instance already up is a no-op. Only needed before an
`ingest-planning` run or a `serve` with planning polling; ingestion otherwise degrades to a WARN.

```bash
scripts/sidecar-start.sh
systemctl --user stop urban-sidecar        # stop
journalctl --user -u urban-sidecar -f      # logs (also data/sidecar.log)
SIDECAR_PORT=8765 URBAN_SIDECAR_PYTHON=/path/to/python scripts/sidecar-start.sh
```

#### `surreal-drop-test-ns.sh` — Sweep leaked test namespaces

See [Test Namespace Hygiene](#test-namespace-hygiene).

#### `check.sh` — One-shot local CI gate

Mirrors `.github/workflows/ci.yml` locally: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the workspace tests. Runs every gate even when an earlier one fails, prints a summary, and exits non-zero if any gate failed.

```bash
scripts/check.sh              # fmt --check, clippy -D warnings, workspace tests
scripts/check.sh --fix        # run `cargo fmt --all` first, then the gates
scripts/check.sh --db         # also run the SurrealDB-gated suite (URBAN_DB_TESTS=1)
scripts/check.sh --fix --db
```

#### `status.sh` — Context snapshot

Read-only "where are we?" snapshot: recent commits, uncommitted-change summary, open debug findings, specs awaiting a build, and the tail of the durable worklog. Never changes anything.

```bash
scripts/status.sh
```

#### `rebuild-anglesey.sh` — Refresh the reference analysis derived layer

Maintainer-specific: written for the maintainers' reference analysis (14 wards of one Welsh council), not a general tool. Refreshes that analysis **without re-fetching external data**: keeps all collected Companies House / Land Registry / enrichment events in `graph_events`, deletes only the broken derived layer (motif alerts + CCOD orphans), and regenerates it with the fixed detector.

```bash
scripts/rebuild-anglesey.sh                 # full run (prompts before deleting)
scripts/rebuild-anglesey.sh --yes           # no prompts
scripts/rebuild-anglesey.sh --skip-backup   # skip the surreal export
scripts/rebuild-anglesey.sh --skip-enrich   # skip OSINT enrichment
scripts/rebuild-anglesey.sh --skip-prune    # derived layer already pruned; resume at step 3
scripts/rebuild-anglesey.sh --detect-secs 180 --ward <ward-code>
```

#### `enrich.sh` — Run OSINT enrichment adapters

Manages the SurrealDB lifecycle, builds the CLI binary, and runs the `enrich` subcommand. Preferred over calling `cargo run enrich` directly when the database is not already running.

```bash
# Run all enabled adapters (starts SurrealDB, runs enrichment, stops SurrealDB)
./scripts/enrich.sh

# Run a specific adapter
./scripts/enrich.sh --source london_gazette
./scripts/enrich.sh --source land_registry_ccod

# Skip SurrealDB management (use when DB is already running)
./scripts/enrich.sh --no-db
./scripts/enrich.sh --source ch_psc --no-db
```

Flags: `--source <name>` (default: `all`), `--no-db`

Note: The London Gazette adapter is budgeted and cursor-tracked — each pass has a wall-clock budget
(`max_pass_secs`, code default 300 s ≈ one ~75-fetch batch; the committed `config.toml` sets 1800)
and an empty cursor looks back at most `lookback_days` (default 30). There is **no deep backfill**:
`compute_published_from` starts an empty cursor at `today - lookback_days` and clamps an older
cursor to the same floor, so the first run covers the same span as every later one. Runs are
incremental with a partial checkpoint, so repeated passes make forward progress.

#### `export_graphml.sh` — Export graph_events WAL to GraphML

Wraps `urban-integrity-cli export-graphml --source events` to build (if needed) and export the full graph directly from the `graph_events` WAL, without requiring `urban-integrity-cli project` to have run first. It still passes `--source events` explicitly even though that is now the CLI default, so the script's behaviour does not move if the default changes again. Output is a timestamped GraphML file in the repo root: `urban-integrity-<YYYYMMDD-HHMMSS>.graphml`. Note the script takes no `--analysis` — like the CLI's `events` path, it exports the `config.toml` default database.

```bash
# Build release binary (if needed) + export
scripts/export_graphml.sh

# Use existing debug build instead of release
scripts/export_graphml.sh --debug

# Skip cargo build entirely (use pre-built binary)
URBAN_CLI=/path/to/urban-integrity-cli scripts/export_graphml.sh
```

---

## Adding a New Motif Detector

1. **Create** `crates/detector/src/motif/your_detector.rs`
2. **Implement** the detector struct with a `check(snapshot: &CsrSnapshot, id_map: &HashMap<u32, String>) -> Vec<MotifResult>` method
3. **Use** `&snapshot.graph[node_idx]` for node weight access (CSR index operator)
4. **Register** in `crates/detector/src/motif/mod.rs`
5. **Add** a benchmark function in `crates/detector/benches/motif_detectors.rs`
6. **Add** a `MotifType` variant in `crates/detector/src/alert/result.rs`

Key patterns from existing detectors:
```rust
// Node weight access — use index operator, NOT node_weight() method
match &snapshot.graph[node_idx as u32] {
    NodeWeight::Entity(e) => { /* ... */ }
    _ => continue,
}

// Neighbor iteration
for &neighbor in snapshot.graph.neighbors_slice(node_idx as u32) { ... }

// Delta-only scan (only re-process modified nodes)
let delta = snapshot.delta_since(prev_baked_at_event);
for &seed_idx in &delta { ... }
```

---

## Adding a Tier-1 Probe

A probe answers one forensic question about the graph and reports a `Finding { probe, verdict,
summary, … }`. All **22** probes — 7 Tier-1, 12 Tier-2 and 3 Cluster-A anchors — live in
`crates/probe/src/`, one file each (the CLI's `probe` subcommand and the workbench both call
into this crate). They are registered in exactly one place,
`run.rs::build_battery`, and dispatched either by `graph::run_gated` (the default) or by
`lib.rs::run_probes` (the deliberately ungated primitive, selected with `--full-battery`). See
[ARCHITECTURE.md § Probe Battery](ARCHITECTURE.md#probe-battery-probe) for what each
one measures.

1. **Create** `crates/probe/src/your_probe.rs` and `pub mod your_probe;` it in
   `lib.rs`.
2. **Implement** the `Probe` trait — `name()`, `tier()` (exactly `"Tier-1"` or `"Tier-2"`),
   `hypothesis()` (one sentence stating the question the probe answers), and
   `async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding`. Override `depends_on()`
   only if the probe genuinely needs another probe's verdict — it defaults to `&[]`, and a name it
   returns that is not a registered probe fails startup.
3. **Register** it in the ordered `Vec<Box<dyn Probe>>` returned by `run.rs::build_battery` — the
   single registration site, which is what makes `probe-oracle`'s coverage reconciliation
   meaningful. Every Tier-1 probe must be registered **before** every Tier-2 probe:
   `reject_interleaved_tiers` fails startup otherwise, because the Tier-1 → Tier-2 gate reads only
   the probes dispatched so far and would judge a Tier-2 probe against an incomplete Tier-1 set.
4. **Read through `urban_integrity_core::persistence::scan`** — never raw SurrealQL (see the guard
   below).

Seven constraints are enforced by tests rather than by convention:

- **`run` is infallible on purpose.** There is no `Result`. A failed query or a missing table is
  caught *inside* `run` and returned as `Verdict::data_gap(...)` or `Verdict::blocked(...)`. A
  `Result`-returning `run` would let the dispatch loop branch on success/failure, which is exactly
  the verdict-conditioned branching the loop must not grow.
- **`run_probes` never branches on a verdict — gating is a separate function, not a flag inside
  it.** `lib.rs::run_probes` remains the unconditional primitive: every registered probe runs, no
  `is_enabled()`, no admission gate, no early return. Verdict gating landed as an *additive* second
  dispatcher, `graph::run_gated`, which `execute()` selects by default (`--full-battery` opts back
  into `run_probes`). `run.rs::dispatch_battery` validates the dependency graph
  **unconditionally, before the mode branch**, so `--full-battery` cannot bypass validation either;
  `dispatch_battery_rejects_an_invalid_graph_under_both_dispatch_modes` pins that on both paths
  using stub probes whose `run()` panics, so a skipped validation shows up as an abort rather than
  a quiet pass. Do not merge the two loops.
- **Registration order is load-bearing, and it is *not* module order.** `IdentityIntegrity`
  (position 2) must run before `SharedExactAddress` (position 4), because the first writes
  `Strategy.identity_blocked` and the second reads it. rustfmt has **`reorder_modules` on by
  default**, so the `pub mod` declarations in `lib.rs` are alphabetised on every format run and say
  nothing about dispatch order — the only place order is expressed is the `vec![...]` in
  `run.rs::build_battery`, and
  `probe_tier1_e2e.rs::identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact`
  is what fails if someone reshuffles it. Never make anything depend on declaration order. Under
  gating, `run_gated` *dispatches* in topological order but fills a slot array indexed by each
  probe's original position, so the returned `Vec<Finding>` still matches the registered battery
  1:1 and in registration order — `execute()`'s downstream `zip` depends on that.
- **A `Coverage` cannot be fabricated.** `Verdict::classify(population, capable, matches,
  hit_threshold, reason)` is the only producer; `Coverage`'s fields are private, so
  `Verdict::Clear(Coverage { population: 5, capable: 3 })` from another module is a compile error
  (E0451), not a runtime assertion. `Verdict`, `Coverage` and `Finding` derive `Serialize` **only** —
  a derived `Deserialize` is generated in the same module as the type, so field privacy would not
  block it and any crate could build an invariant-violating verdict straight from JSON. If a later
  phase needs to read a verdict back, it must hand-write a `Deserialize` that re-runs `classify`'s
  validation. `verdict_does_not_implement_deserialize_owned` pins this.
- **Never report `Clear` on a population of zero.** `classify(0, 0, …)` satisfies `capable ==
  population` and produces a well-formed `Clear` — a clean bill of health for a set that was never
  measured, which is the whole hazard class the verdict vocabulary exists to remove. Short-circuit
  to `DataGap` first. `SharedExactAddress` was the last probe still inheriting Python's version of
  this and was fixed on 2026-08-23.
- **No raw SurrealQL in a probe body.** `probe_tier1_e2e.rs::tier1_probes_issue_no_raw_queries`
  scans the production portion of every file in `crates/probe/src/` (everything before the first
  `#[cfg(test)]`) and fails on a `.query(` call. `run.rs`, `lib.rs` and `registry.rs` are exempt —
  `run.rs` opens the two connections, the other two hold no probe body. Fixture queries inside `#[cfg(test)]` blocks
  are fine.
- **Zero new `ALLOWLIST` entries.** `allowlist_gained_no_entries_this_phase` pins the query-shape
  guard's allowlist at its current baseline (8 — see [Query Shapes That Lose Rows
  Silently](#query-shapes-that-lose-rows-silently) for why it moved from 6, and why that was a
  narrowing rather than a widening). A probe that cannot express its read safely is a signal to
  extend `scan`, not to widen the allowlist.

`PrecomputedAlerts` shows the pattern the whole battery turns on: it reads
`detector_runs:current` (written by `GraphCoordinator` on the detector's success path only, and
never as a `motif_count: 0` row standing in for "never ran") so that zero alerts can be reported as
`Clear` when the detector ran and as `DataGap` when it did not. Those are different facts and the
report must not merge them.

Run the battery locally with `cargo run -p urban-integrity-cli -- probe`; the DB-gated e2e test is
`URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test probe_tier1_e2e`.

---

## Adding a New OSINT Enricher Adapter

> **ENR-01 isolation invariant (revised when the PPD adapter landed):** `crates/enricher` must not import the ingestor's *pipeline* — clients, pollers, mappers, or ingest event types. CH stream-based adapters use `urban_integrity_core::stream` primitives, not `crates/ingestor` code. Since commit `d3a4ec7`, `crates/enricher/Cargo.toml` does carry a `urban-integrity-ingestor` dependency for shared address parsing. The intent was to hold that to `address::{extract_house_number, normalise_postcode}`, and **the tree has already exceeded it** — there are three import sites, not two: `crates/enricher/src/adapters/land_registry_ppd.rs` (`address::{extract_house_number, normalise_postcode}`), `crates/enricher/src/land_flip.rs` (`address::normalise_postcode`) and `crates/enricher/src/land_flip.rs` again (`address::planning::parse_planning_address`). The third reaches into the ingestor's planning-address parser, which is not a normalisation helper. Treat that as the current ceiling and shrink it rather than adding a fourth. `crates/enricher/tests/hot_path.rs` still imports zero ingestor symbols.
>
> **Exact-match-only invariant (since 2026-06-13, commit `43675f8`):** The enricher performs **exact-identifier joins only** — there is no in-crate fuzzy entity resolver. `crates/enricher/src/resolver.rs` (which held `composite_score`, Jaro-Winkler, and Double-Metaphone) was deleted, and the `strsim` and `rphonetic` dependencies were dropped. Identity links are created only from exact joins on a stable identifier (e.g. Companies House `company_number`). PEP/sanctions name screening is delegated to the **external OpenSanctions/yente sidecar**, not done in-crate. Do not reintroduce name-only fuzzy matching — uncorroborated name matches previously produced tens of thousands of false positives (see the Enricher Adapter Reference notes for charity and EC). The one deliberately bounded exception is the OCOD↔ICIJ matcher, which is hard-capped at 0.65 and forced to `pending_review` — it can never auto-link.

1. **Create** `crates/enricher/src/adapters/your_adapter.rs`
2. **Implement** the `EnricherAdapter` trait (defined in `crates/enricher/src/adapters/mod.rs`):
   ```rust
   use async_trait::async_trait;
   use surrealdb::{engine::any::Any, Surreal};
   use tokio::sync::mpsc::Sender;
   use urban_integrity_core::graph::types::GraphMutation;
   use crate::adapters::{EnricherAdapter, EnrichmentStats};

   pub struct YourAdapter { /* credentials, config */ }

   #[async_trait]
   impl EnricherAdapter for YourAdapter {
       fn name(&self) -> &'static str { "your_source" }

       fn is_enabled(&self) -> bool {
           // return false when credentials are absent — adapter is skipped silently
           !self.api_key.is_empty()
       }

       async fn run(
           &self,
           db: &Surreal<Any>,
           mutation_tx: &Sender<GraphMutation>,
       ) -> anyhow::Result<EnrichmentStats> {
           // 1. Query SurrealDB for candidate nodes
           // 2. Call external data source
           // 3. Join on a stable identifier (e.g. company_number) — exact match only
           // 4. Score results with ConfidenceScore::new(value)?
           // 5. Use ConfirmedLink::try_new (score >= 0.85) → emit GraphMutation via mutation_tx
           // 6. Use CandidateLink::try_new (score >= 0.70) → write to identity_links as pending_review
           // 7. Return EnrichmentStats
           Ok(EnrichmentStats::default())
       }
   }
   ```
3. **Expose** the new adapter in `crates/enricher/src/adapters/mod.rs` (add `pub mod your_adapter;`)
4. **Register** the adapter in `build_default_adapters()` in `crates/enricher/src/engine.rs` — add `Box::new(YourAdapter::new(...))` to the returned vec. This is the single registration site for all adapters.

   > **For CH stream-based adapters (PSC, Disqualified Directors):** Use `urban_integrity_core::stream::run_ch_stream` and `SurrealStreamCursor` from `crates/core` — do NOT copy the reconnect loop or cursor from `crates/ingestor`. The stream primitive was extracted to core (CHA-03) specifically so adapters can consume CH streams without importing the ingestor pipeline (ENR-01). Gate the adapter with `ChEnricherConfig::is_enabled()` from `crates/core::config::ChEnricherConfig`.

5. **Write to `identity_links`** using the flat string pattern for `evidence_json` (ENR-06):
   ```rust
   // CORRECT — evidence_json is always a String, never serde_json::Value
   let evidence = serde_json::to_string(&my_evidence_struct)?;
   // record.evidence_json = evidence;
   ```
6. **Use the UPDATE subquery guard** for all `identity_links` status mutations (SurrealDB issue #7018):
   ```sql
   UPDATE (SELECT id FROM identity_links WHERE id = $id) SET status = $status
   ```
   Never use `UPDATE identity_links SET ... WHERE id = $id` directly — that form triggers the bug.
7. **Never discard a SurrealDB write `Result`.** A `let _ = db.query(...)` write compiles and "works" while silently losing every row (the ICIJ adapter shipped this way and wrote 0 rows for its entire life while logging success). Always `.await?` **and** consume the response with `.take(...)`/`.check()` so per-row schema errors surface. Batch writes; test at least one row round-trip against the real `SCHEMA_DEF` table definition.
8. **Test** using the pattern in `crates/enricher/tests/`: `cargo test -p urban-integrity-enricher`. DB-gated tests take their throwaway namespace from `urban_integrity_test_support::TestNs::connect("integrity")` — never a hand-rolled `test_{uuid}` — so parallel/CI runs cannot collide and the namespace is removed on drop.

### Confidence thresholds

| Threshold | Constant | Type | Outcome |
|-----------|----------|------|---------|
| `>= 0.85` | `AUTO_LINK_THRESHOLD` | `ConfirmedLink` | Hot graph mutation emitted via `mutation_tx` |
| `>= 0.70` | `CANDIDATE_THRESHOLD` | `CandidateLink` | Written to `identity_links` as `pending_review` for analyst review |
| `>= 0.50` | `SUB_CANDIDATE_THRESHOLD` | `SubCandidateLink` | Read-only — surfaced on `entity --deep` only (D-13), never stored in `identity_links` |
| `< 0.50` | — | discarded | Neither stored nor emitted |

`ConfirmedLink::try_new`, `CandidateLink::try_new`, and `SubCandidateLink::try_new` (all defined in `crates/enricher/src/confidence.rs`) each return `None` for sub-threshold scores, making the wrong promotion path unreachable by construction (ENR-11). `ConfidenceScore::new(value)` enforces the `[0.0, 1.0]` range and returns `Err` otherwise.

### Exact-match scoring policy

Adapters assign confidence from the strength of the joined identifier, not from lexical name similarity:

- An exact `company_number` join (the charity, CCOD, and EC company-donor paths) is the trustworthy link class and scores at or near 1.0 (auto-linked).
- Name-only matches with no corroborating field (DOB, address, company number) are **not persisted** — they are discarded rather than written to `identity_links`. This is enforced per-adapter; see the EC and charity adapter notes below for the history. (Exception: the OCOD↔ICIJ matcher persists name-only matches, but hard-capped at 0.65 and forced to `pending_review`.)
- External name screening (PEP/sanctions) is performed by the OpenSanctions/yente sidecar, which applies its own name capping; the in-crate adapter only consumes its scored results.

---

## EnricherEngine Constructor

`EnricherEngine::new` takes four parameters plus the adapter list and the analysis nation:

```rust
pub fn new(
    db: surrealdb::Surreal<surrealdb::engine::any::Any>,
    mutation_tx: Sender<GraphMutation>,
    config: EnricherConfig,
    surreal_config: SurrealConfig,   // ← 4th param
    adapters: Vec<Box<dyn EnricherAdapter>>,
    analysis_nation: Option<Nation>,
) -> Self
```

`surreal_config` is a `SurrealConfig` from `urban_integrity_core::config`. It is stored on the engine so that `reauth()` can sign in directly on `self.db` (not a clone) when the SurrealDB session token expires during a long enrichment run. Passing a clone to `reauth` is incorrect — `Surreal::clone()` creates an independent session with a new UUID, so refreshing the clone does not refresh the engine's own session.

The CLI `enrich` command and the background enrichment task both construct the engine via `build_default_adapters()`. Note that the `SurrealConfig` passed in must carry the **resolved analysis database** (via `resolve_analysis_db`), not the config default — otherwise an `--analysis` run would silently switch back to `main` at the first periodic re-auth:

```rust
let mut surreal_config = config.surreal.clone();
surreal_config.database = crate::db::resolve_analysis_db(&config).await?;

let adapters = build_default_adapters(&config.enricher, std::sync::Arc::new(db.clone()));
let analysis_nation = config.analysis_nation()?;
let engine = Arc::new(EnricherEngine::new(
    db,
    engine_tx,
    config.enricher.clone(),
    surreal_config.clone(),
    adapters,
    analysis_nation,
));
```

---

## Enricher Test Suite

`crates/enricher/tests/` contains 11 test modules (plus `fixtures/`). The table below describes each module's scope and gating.

| File | Gating | Scope |
|------|--------|-------|
| `hot_path.rs` | always (no DB) | ENR-01/ENR-08/ENR-11 compliance — imports zero ingestor symbols |
| `psc_adapter.rs` | offline tests always; DB tests behind `URBAN_DB_TESTS=1` | PSC fixture parsing, matcher, confidence assignment, `write_identity_link` error propagation |
| `disqualified_adapter.rs` | offline tests always; DB tests behind `URBAN_DB_TESTS=1` | Disqualified Directors fixture parsing, DOB logic, edge-meta, `write_disq_identity_link` error propagation |
| `engine_coverage.rs` | `URBAN_DB_TESTS=1` | `EnricherEngine::new()` / `run_all_enabled()` / `run_source()` / `start_background()` / `reauth()` coverage; session-independence regression |
| `ccod_adapter.rs` | `URBAN_DB_TESTS=1` | HMLR CCOD CSV streaming, `company_number` validation, DoS guard |
| `gazette_adapter.rs` | `URBAN_DB_TESTS=1` | London Gazette insolvency notice parsing and status mutation |
| `evasion_detector.rs` | `URBAN_DB_TESTS=1` | Disqualified Director Evasion post-step |
| `property_accumulation.rs` | `URBAN_DB_TESTS=1` | Property Accumulation Anomaly detector (PROP-03) |
| `jurisdiction.rs` | always (no DB) | Source-coverage gate: each adapter declares the nations its source actually covers, and `adapter_admitted` refuses the rest. CCOD is the dangerous one — its row filter is `company_number` alone with no geography test, so a Scottish analysis would otherwise receive real English property as local corroboration |
| `donation_correlation_endpoint.rs` | `URBAN_DB_TESTS=1` | POL-04's donor→contract join. `awarded_to` is `FROM entity TO contract`, so the detector's `WHERE out = $entity_id` asked "is this donor a contract?" — always false. Asserts on an emitted alert, not on query text, because the id form (`identity_links.source_node_id` holds a bare `GB-COH:…`, `in` holds a record link) is the other half of the same silence |
| `opencorporates_targets.rs` | `URBAN_DB_TESTS=1` | Target selection. `WHERE company_number IS NOT NULL` excluded nothing — SurrealDB distinguishes an **absent** field (`NONE`) from an explicit `NULL`, and the projector omits the key entirely. Measured live 2026-08-16: `IS NOT NULL` returned all 72,123 entity rows; `IS NOT NONE` returned the 14,720 that carry a number |

DB-gated tests isolate themselves in **per-run namespaces** obtained from `urban_integrity_test_support::TestNs::connect("integrity")` — namespace naming is owned by `NsGuard`, which also removes the namespace on drop — so parallel test binaries and repeated CI runs never collide on shared tables. This eliminated a class of CI flakes; see [Test Namespace Hygiene](#test-namespace-hygiene).

Run the enricher test suite:

```bash
# Offline tests only (no SurrealDB required)
cargo test -p urban-integrity-enricher

# Full suite including DB integration tests
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher
```

### Session-expiry regression tests

`engine_coverage.rs` and the `psc_adapter.rs` / `disqualified_adapter.rs` modules each contain regression tests for the session-expiry write failure discovered on 2026-06-07. Prior to the fix, `write_identity_link` and `write_disq_identity_link` swallowed all DB errors (`warn! + Ok(())`), causing silent data loss when the SurrealDB token expired mid-run.

The regression tests use a purposely unsigned connection (no `signin` or `use_ns`/`use_db` call) to reproduce the 401 Unauthorized failure mode:

```rust
// Reproduces the exact failure: connect without signing in
let db = surrealdb::engine::any::connect("http://localhost:8000").await?;
// Intentionally skip signin and use_ns/use_db

let result = write_identity_link(&db, &record).await;
assert!(result.is_err(), "write_identity_link must propagate DB errors");
```

`engine_coverage.rs::test_engine_reauth_refreshes_own_session` verifies that `engine.reauth()` calls `signin()` on `self.db` directly, not on a clone. `test_surreal_clone_invalidate_leaves_original_usable` documents the `Surreal::clone()` independent-session invariant that was the root cause.

### Proving a privacy invariant needs a `compile_fail` **pair**

`crates/enricher/src/confidence.rs` keeps `ConfirmedLink`'s fields private so a sub-threshold link
cannot be built by struct literal, skipping `try_new`'s threshold check (T-16-05). The doctest that
proves it is two snippets, not one:

- a ```` ```compile_fail ```` snippet that builds `ConfirmedLink { score, record }` directly, and
- an identical snippet that goes through `try_new` and **must compile**.

A `compile_fail` doctest passes if the snippet fails to compile for *any* reason — a typo or a bad
import satisfies it exactly as well as the privacy error it is meant to demonstrate. The
must-compile twin is what attributes the failure to field privacy rather than to anything
incidental in the setup. Copy the pair when pinning any construction invariant; the same reasoning
is why `Verdict`/`Coverage` derive `Serialize` only (see [Adding a Tier-1
Probe](#adding-a-tier-1-probe)).

---

## Enricher Adapter Reference

The `crates/enricher` crate ships **fourteen adapters**, all registered in `build_default_adapters()` in `crates/enricher/src/engine.rs` (`adapters/download.rs` sits alongside them but is not one — it holds the size-capped body readers the bulk adapters share, see Standing Rule 10): `ch_psc`, `ch_disqualified`, `land_registry_ccod`, `land_registry_ocod`, `london_gazette`, `charity_commission`, `opensanctions`, `icij`, `electoral_commission`, `council_registers`, `opencorporates`, `ppd`, `mutuals_register`, `planning_identity`. After all adapters run, **six post-adapter detectors** execute: `evasion` (D-10), `property_accumulation` (PROP-03), `offshore_shadow` (FUZ-05), `charity_crossover` (FUZ-06), `donation_correlation` (POL-04), and `land_flip` (FLIP-02).

The TOML blocks reproduced below are a mix of committed configuration and code defaults, and the
difference matters when you go looking for a key. Of them, only `[enricher.land_registry]`,
`[enricher.ocod]`, `[enricher.ppd]` and `[enricher.london_gazette]` appear in the committed
`config.toml` — whose complete contents are `[surreal]`, `[graph]`, `[workbench]`, `[ingest]`,
`[[ingest.planning_portals]]` (×17), `[detector]`, `[enricher]`, `[enricher.ch]`,
`[enricher.land_registry]`, `[enricher.ocod]`, `[enricher.london_gazette]` and `[enricher.ppd]`,
and nothing else. The blocks shown for `charity_commission`, `opensanctions`, `icij`,
`electoral_commission`, `council_registers` and `opencorporates` are **illustrative**: they
reproduce the Rust defaults from `crates/core/src/config.rs`, and writing one into `config.toml`
is how you override those defaults — do not expect to find it there already. [CONFIGURATION.md
§ `[enricher]`](CONFIGURATION.md#enricher--enrichment-engine) is the authoritative reference for
every key, default, and `URBAN__*` override.

### `ch_psc` — PSC Beneficial Ownership (CHA-01)

Consumes the Companies House PSC (Persons of Significant Control) streaming endpoint. Requires the `[enricher.ch]` API key — a separate key from `[ingest] ch_api_key`. Both can be the same physical CH API key, but they serve different purposes and are configured independently:

| Config key | Env var | Purpose |
|---|---|---|
| `[ingest] ch_api_key` | `URBAN__INGEST__CH_API_KEY` | Companies House **Streaming API** (long-lived NDJSON) |
| `[enricher.ch] api_key` | `URBAN__ENRICHER__CH__API_KEY` | Companies House **REST API** for enricher adapters |

Disabled (`is_enabled() == false`) when `[enricher.ch] api_key` is absent or empty.

### `ch_disqualified` — Disqualified Directors (CHA-02)

Consumes the CH Disqualified Directors streaming endpoint. Uses the same `[enricher.ch] api_key` as the PSC adapter (same graceful-degradation gate). Registered after `ch_psc`; the Disqualified Director Evasion post-step (`src/evasion.rs`) runs after this adapter completes.

### `land_registry_ccod` — HMLR CCOD Bulk CSV (PROP-01)

Downloads the monthly Commercial and Corporate Ownership Data (CCOD) bulk ZIP from the HM Land Registry "Use Land and Property Data" service. Streams the CSV with bounded memory using `tokio::task::spawn_blocking` + `csv::Reader`. Emits `Property` nodes and `OwnsProperty` edges for companies matched by `company_number`. Cached locally to avoid re-downloading within the monthly refresh interval.

Configuration:

```toml
[enricher.land_registry]
api_key = ""                     # URBAN__ENRICHER__LAND_REGISTRY__API_KEY
cache_dir = "data/ccod"          # local cache directory for downloaded ZIPs
ccod_refresh_interval_days = 30  # re-download interval (HMLR monthly cadence)
```

Security constraints: `api_key` is never logged (only `api_key_present = true/false`); ZIP files larger than 2 GiB are rejected (DoS guard); `company_number` values from CSV are validated via `is_valid_ch_number` before any DB write.

### `land_registry_ocod` — HMLR OCOD Overseas Ownership (PROP-04/05/06)

Downloads the monthly Overseas Companies that Own (UK) property Data (OCOD) bulk ZIP — a near-exact clone of the CCOD adapter with these deltas:

- Records carry `proprietor_name` and a **canonical `country_incorporated`** (canonicalised via `src/country_canonical.rs` *before* any jurisdiction filter — a PROP-05 ordering invariant). Fields are parsed by header name, never by column index.
- ROE `OE######` proprietors are keyed with the `GB-ROE:` node form; UK company numbers keep `GB-COH:`. Emits `OffshoreEntity`/`Property` nodes and `OwnsProperty` edges.
- A bounded OCOD↔ICIJ name-only matcher (PROP-06) is **hard-capped at 0.65 and forced to `pending_review`** — it must never auto-link.
- Same 2 GiB ZIP guard and never-log-the-key rules as CCOD.

Configuration:

```toml
[enricher.ocod]
api_key = ""                    # URBAN__ENRICHER__OCOD__API_KEY; blank inherits the
                                # shared [enricher.land_registry] key (one HMLR account
                                # licenses both CCOD + OCOD)
cache_dir = "data/ocod"
ocod_refresh_interval_days = 30
```

Disabled until a non-empty HMLR key resolves (own key, else the shared `land_registry` key).

### `ppd` — HMLR Price Paid Data staging (FLIP-01)

Downloads **yearly** Price Paid Data files (`pp-{year}.txt`, for the last `years_back` years — the 5.1 GB national `pp-complete.txt` is never targeted) from the HMLR open-data endpoint (OGL v3, no API key). Filters rows at load time to in-scope LPA postcode districts and writes them to the `ppd_transactions` staging table via **idempotent UPSERT keyed on the PPD transaction UUID**. This is a staging-only adapter — raw PPD rows are never emitted as `GraphMutation`s (mirrors the ICIJ staging pattern). The staged rows feed the land-flip detector.

The postcode-district allowlist (D-04) is either explicit (`postcode_districts = ["XX1", "XX2"]`) or derived dynamically from existing `Location`-node postcodes when absent.

Configuration:

```toml
[enricher.ppd]
enabled = true             # URBAN__ENRICHER__PPD__ENABLED (the Rust default is false;
                           # enable it in your config.local.toml)
years_back = 5             # URBAN__ENRICHER__PPD__YEARS_BACK
# cache_dir = "data/ppd"
# The allowlist is set EXPLICITLY rather than derived (D-04): deriving from Location
# nodes yields 3,202 districts — CCOD/OCOD write UK-wide registered-office addresses
# into `location` — which passes essentially every PPD row in the country and
# previously wedged SurrealDB. List the outward codes covering your own LPA; the
# values below are fictional placeholders.
postcode_districts = ["XX1", "XX2", "XX3"]
```

Guards: Content-Length > 500 MiB → WARN + skip; all field values written via parameterised `.bind()`; rows chunked at 25 000 with a `yield_now()` per chunk (RocksDB flush — CCOD OOM lesson).

> **Idempotency check without a full re-run:** idempotency is structural (UPSERT on the transaction UUID). Confirm it via an unchanged `count()` or a 1-year re-run — never a full national re-ingest, which thrashes SurrealDB's background tasks for hours.

### `london_gazette` — Corporate Insolvency Notices (PROP-02)

Polls the public Gazette REST API for winding-up / liquidation notice codes (2452, 2454, 2410) and emits `GraphMutation::UpdateNode` to set `EntityStatus::InLiquidation` on matched entities. No API key required — always enabled.

**Pass budget and lookback:** each enrichment pass has a wall-clock budget (`max_pass_secs`, code default 300 s ≈ one ~75-fetch batch; the committed `config.toml` sets **1800**), checked at the 75-fetch boundary. Cursor resolution: a partial checkpoint resumes mid-backfill; a completed-pass cursor is clamped to the lookback window; an empty cursor starts at `today - lookback_days` (default 30). Runs are incremental — repeated passes make forward progress without unbounded crawling.

**Rate-limit behaviour (`gazette_fetch_json`):** The Gazette REST API enforces an IP-level rate limit of approximately 100 JSON-LD fetches per ~82-second window; exceeding it returns HTTP 200 with an empty body (not a 429). `gazette_fetch_json` retries up to 3 times with a 90-second back-off on non-JSON responses. Separately, the adapter pauses 300 seconds every 75 JSON-LD fetches to proactively reset the window. A 2-second pause is inserted between search-result pages.

The client uses `connect_timeout(30s)` and `cookie_store(true)` to maintain the Gazette JSESSIONID across pages.

**Scope:** InLiquidation only (notice codes 2452/2454/2410). Dissolution is intentionally excluded — CH streaming already delivers dissolution via `EntityStatus::Dissolved`.

Configuration — the block is **active** in the committed `config.toml` and overrides the code
defaults (`max_pass_secs` 300, `lookback_days` 30, both in `crates/core/src/config.rs`). Override
again via `URBAN__ENRICHER__LONDON_GAZETTE__MAX_PASS_SECS` / `__LOOKBACK_DAYS`:

```toml
[enricher.london_gazette]
max_pass_secs = 1800   # raised from the 300 default on 2026-08-13: a 300 s pass cleared
                       # roughly one results page (314–415 s observed, overrunning the
                       # budget before reaching a boundary), so draining a backlog cost a
                       # pass per page. Still checkpointed at every page/75-fetch boundary,
                       # so it stays resumable and bounded — it just does more per run.
lookback_days = 30
```

### `charity_commission` — Charity Commission Bulk Register (FUZ-02)

Downloads the Charity Commission for England and Wales bulk register from the Azure blob CDN (`publicextract.*` schema, OGL v3, no API key required). Produces `Charity` nodes and `TrusteeOf` edges. Identity links are only created via exact `company_number` join (charity registered company number == an Entity's `GB-COH` company number) — fuzzy name matching was removed because charity data carries no DOB or address corroboration (name-only matching produced ~87k false positives before being purged on 2026-06-11).

Configuration:

```toml
[enricher.charity_commission]
enabled = true
cache_dir = "data/charity"
refresh_interval_days = 30
```

### `opensanctions` — PEP and Sanctions Screening (FUZ-03)

Screens director/entity nodes against the OpenSanctions/yente self-hosted sidecar at `localhost:8766`. Requires explicit CC-BY-NC licence acknowledgement (`non_commercial_confirmed = true`) — the adapter returns `Err` at runtime if this is not set. All name matching is performed by the external yente service; the adapter consumes its scored results rather than running any in-crate fuzzy matcher.

Name-only matches (no DOB or nationality) are capped at 0.65 regardless of yente score, preventing common-name false positives from reaching the auto-link tier.

Configuration:

```toml
[enricher.opensanctions]
enabled = false
non_commercial_confirmed = false
endpoint = "http://localhost:8766"
batch_size = 50
```

### `icij` — ICIJ Offshore Leaks (FUZ-04)

One-shot idempotent load of all four ICIJ Offshore Leaks datasets from `full-oldb.LATEST.zip` (~810k entities). All data is written exclusively to the `icij_offshore_entity` and `icij_offshore_link` tables (D-09 ODbL isolation — prevents mixing with OGL-licensed tables). If `icij_offshore_entity` already contains rows, the load is skipped (D-10). ICIJ data is loaded into isolated tables and surfaced read-only, not auto-linked by name (the sole bounded consumer is the OCOD↔ICIJ matcher, capped at 0.65 / `pending_review`).

Write path (post-fix, PR #18): rows are shaped to match the `SCHEMA_DEF` table definitions (`dataset` is a **required** string carrying ICIJ's `sourceID`; links use `from_node_id`/`to_node_id`, not the raw CSV `node_id_start`/`node_id_end` names) and written via **batched `INSERT IGNORE` statements sized by the AIMD `BatchController`** — one query per batch, never per-row round-trips. Every insert response is consumed with `.take(0)` and error-contexted, so a schema mismatch **fails loud** instead of silently writing nothing. The pre-fix adapter discarded write errors with `let _ =` and omitted the required `dataset` field — it logged "814k written" against an empty table for its entire life. Do not regress this.

Other behaviours: ZIP parsing runs in `spawn_blocking` with bounded memory; 2 GiB download guard; all CSV column access is by header name (ICIJ schemas drift between releases — the `README.txt` inside the ZIP is logged at INFO on first run so the operator can verify actual headers); rows with empty `node_id` or `name` are skipped with WARN.

Configuration:

```toml
[enricher.icij]
enabled = true             # code default — ODbL open data, no key required
cache_dir = "data/icij"
```

### `electoral_commission` — EC Donations (POL-01)

Downloads the Electoral Commission donations CSV (OGL v3, no API key required). Performs an exact `company_number` join for **company donors** (confidence 1.0, auto-linked). **Individual-donor name-only matching is disabled** — an individual donation carries no DOB, company number, or address, so any director↔donor match would be structurally uncorroborated. Before being disabled (2026-06-13), the individual-donor path wrote ~3,513 false-positive director→party links (93% of `identity_links`); company-number exact joins are the only trustworthy EC link class and are unaffected. DonorName is stored as a normalised slug only — raw names are never persisted (PII minimisation). Recipients are emitted as `PA:<slug>` political-actor nodes so `DonatesTo` edges project cleanly.

Two cross-site contracts this adapter shares with the POL-04 donation-correlation detector, both
of which silently produced nothing until 2026-08-14:

- **`AcceptedDate` is `DD/MM/YYYY`, not ISO.** Both sites parsed `%Y-%m-%d`; the adapter
  discarded the failure with `.ok()`, so the typed `accepted_date` was `None` for every EC record
  ever ingested, with no log line. Both now route through `crate::ec_date::parse_ec_date`, which
  accepts the feed format **and** ISO — deliberately, because rows written before the fix hold
  `DD/MM/YYYY` in `evidence_json` while rows written after hold ISO (`ec_date::to_iso`). Anything
  reading `evidence_json.accepted_date` must use `ec_date`, not a local `parse_from_str`.
- **The EC publishes registered legal party names.** `data/party_authority_control.json` keys on
  colloquial ones, so `"Conservative and Unionist Party"` never matched `"Conservative Party"` —
  a two-word difference that normalisation cannot bridge — and every Conservative donation fell
  out at the detector's party gate. `_meta.name_variants` had always recorded the mapping;
  nothing read it. `PartyAuthorityMap::from_json_str` now populates the alias table, so prefer it
  over a bare `serde_json::from_str` — `aliases` is `#[serde(skip)]`, and deserialising directly
  degrades the lookup back to exact-plus-normalised without complaint.
- **The shipped party map is placeholder data.** `data/party_authority_control.json` carries a
  `_meta.source` of `"Open Council Data UK (opencouncildata.co.uk) — 2025 post-election"`, but its
  44 LPA codes are four perfectly contiguous runs — `E06000001`–`E06000009`, `E07000001`–`E07000015`,
  `E08000001`–`E08000006`, `E09000001`–`E09000014` — handed out party by party, which is not how
  real GSS codes distribute across political control. Every code is English; there is **no Welsh
  coverage at all** — and the detector does not treat that absence as a reason to stay silent.
  Read `run_donation_correlation_detector` (`crates/enricher/src/donation_correlation.rs`) rather
  than assuming an LPA gate: the contract query is `SELECT meta.effective_from FROM awarded_to
  WHERE in = type::record('entity', $entity_id) LIMIT 1000` — filtered by **donor only**, with no authority predicate —
  and the ±730-day donation-to-contract window is the only further test. `lpa_codes` is then
  iterated to emit **one alert per LPA the party controls**, with `lpa_code` used purely as a label:
  it appears in the alert description, in `result_json`, and in the `compute_motif_hash` dedup key.
  The map therefore never gates detection; the only skip paths are `lookup_lpa_codes` returning
  `None` (party absent from the file) or an empty list (party controls no LPA). The consequence is
  worse than silence — once a party name resolves, POL-04 fires on an Anglesey contract and
  attributes it to an unrelated English LPA. Fixing the parsers above makes POL-04 fire; it does
  not make this file real. Replace it with genuine Open Council Data before reading any POL-04
  output as a finding.

Configuration:

```toml
[enricher.electoral_commission]
enabled = true
cache_dir = "data/electoral"
```

### `council_registers` — Council Registers of Interest (POL-02)

Scrapes ModernGov council registers of members' interests via the Scrapling sidecar (`localhost:8765`). Extracts councillor declarations and cross-references company names against the local director/entity graph. Disabled by default; requires both `enabled = true` and at least one LPA code in `lpa_codes`. The former name-only fuzzy fallback was removed — only exact-identifier corroboration produces a persisted link.

Configuration:

```toml
[enricher.council_registers]
enabled = false
lpa_codes = []   # e.g. ["X06999001"]
```

### `opencorporates` — Cross-Jurisdiction Lookup (POL-03)

Queries `api.opencorporates.com` for high-P_c / actively-alerted entities, writing cross-jurisdiction identity links. Rate-limited to 1 req/s via `governor`. Requires explicit ODbL licence acknowledgement (`oc_open_data_confirmed = true`) — the adapter returns `Err` at runtime if this is not set. Monthly quota is tracked in the `oc_quota` SurrealDB table.

Configuration:

```toml
[enricher.opencorporates]
enabled = false
oc_open_data_confirmed = false
monthly_limit = 200  # free tier; 500 requires the paid Essentials tier
```

### `planning_identity` — Planning Applicant ↔ Companies House Join (IDN-05)

Closes the gap between `identity_links` and the graph the detectors actually read. Links
`director:{slug}` planning-applicant Entity nodes to existing Companies-House-sourced Entity /
Officer nodes when their **normalised names are identical** — `normalise_name(a) == normalise_name(b)`
is the only comparison performed anywhere in the module. No API key, no network: it re-joins data
already in the graph.

On every match it writes **both** an `identity_links` audit row **and** a real `Directs` graph edge.
That double write is the point: the CSR-snapshot motif detectors read only `graph_events`
`EdgeAdded` rows and never `identity_links`, so an `identity_links` row without a graph edge is
invisible to detection.

Two safety properties, both structural rather than conventional:

- **Ambiguity resolves to no match.** Where several CH-sourced candidates share a normalised name,
  the applicant matches *nothing* — never an arbitrary pick. This mirrors
  `crates/ingestor/src/backfill/ch.rs`'s `MatchVerdict::Ambiguous`.
- **`director:unknown-applicant` is never a candidate.** The placeholder mega-node for unattributed
  planning applications is excluded before the lookup (`is_planning_candidate`); matching it against
  anything would be meaningless.

`write_identity_link(...).await?` is never discarded here — the audit row is the record of truth
this adapter protects (Standing Rule 1). The `mutation_tx.send` uses the same `let _ =` pattern as
every sibling adapter, because the real net is downstream: `wal_writer::drain_to_wal` fails the
whole enrichment run on the first real `graph_events` write failure, and a dropped channel send only
occurs once the channel is already closed (a shutdown condition), never as silent per-row loss.

### `mutuals_register` — FCA Mutuals Public Register

A **registered society** is a corporate body that is *not a company*: constituted under the
Co-operative and Community Benefit Societies Act 2014, registered with the FCA as registrar, and
carrying a number with a letter suffix (`22360R`) unrelated to any Companies House number. Before
this adapter the entire class was invisible to the engine, through two independent blind spots —
Companies House ingest cannot see them (they are not on that register), and the Charity Commission
adapter cannot either (a society with charitable rules is an *exempt charity*, recognised by HMRC
and absent from the CC register).

The class is not marginal. Measured live 2026-08-11, ClwydAlyn Housing (`22360R`) held 52 Anglesey
planning applications and reached the graph as a bare applicant string with no number, no accounts
and no ownership data behind it; `north-wales-housing-association-ltd` (18) and `grwp-cynefin` (14)
sat in the same position.

What it does: each society becomes an `Entity` node keyed **`GB-MUT:{registration}`**, carrying its
registered address as a `Location` node plus a `LocatedAt` edge. The node and edge *shape* match
what `ch/mapper.rs` emits for companies (`NodeWeight::Location` + `EdgeWeight::LocatedAt`); **the
key derivation does not.** The adapter hand-rolls `LOC:{postcode_without_spaces}:{address[..40]}`
instead of calling `address::normalise_address`, so a society and a company at the same registered
address land on different `Location` nodes — `LOC:EC1A1BB:1 High Street` versus `LOC:EC1A1BB:1` —
and never cluster. Societies are therefore **not** yet visible to `AddressCluster` on the same
terms as companies, whatever the in-code comment says; see [the `Location` keyspace hazard
note](#python-forensic-scripts--shell-helpers) for the worked example and the two sibling
keyspaces.
`Society Status` maps `Registered` / `Deregistered` onto the society-world equivalent of active and
dissolved.

What it deliberately does **not** do: create identity links. `planning_identity.rs` already joins
planning applicants to any non-`director:`-keyed Entity node by exact `normalise_name` equality, so
the join happens on the next enrichment pass through code already reviewed for that risk.
Duplicating it here would mean two matchers to keep in agreement.

Two source gotchas:

- The FCA publishes the whole register as **one unauthenticated CSV on Azure blob storage** — no API
  key, no rate limit, no scraping, no sidecar. `mutuals.fca.org.uk` itself is an HTML search form;
  the bulk file linked from its footer is the only sane ingest path.
- **The file is Windows-1252, not UTF-8.** Byte `0x92` (a curly apostrophe, common in names such as
  `Working Men's Club`) is not valid UTF-8 and aborts a strict decoder mid-file. Decoding is
  lossy-by-design: a mangled apostrophe is survivable, silently losing the tail of the register is
  not.

See [CONFIGURATION.md § `[enricher.mutuals]`](CONFIGURATION.md#enrichermutuals--fca-mutuals-public-register-adapter)
for the config block.

### Post-adapter detector: `land_flip` — Pre-Permission Land Flip (FLIP-02..05)

Not an adapter — a post-adapter detector (`src/land_flip.rs`) that runs after all adapters, or standalone via `enrich --source land-flip`. For each in-scope planning application it runs **three mandatory gates** against staged `ppd_transactions`:

1. **Gate 1 — structural address:** `postcode_norm` AND `paon_norm` AND `street_norm` must all match; a postcode-only match produces zero candidates (FLIP-03).
2. **Gate 2 — temporal window:** the transfer date must fall in TIER_1 (inside the application window), TIER_2 (0–12 months before `received_date`), or TIER_3 (12–24 months before). Older, or no anchor date → no candidate.
3. **Gate 3 — connection corroboration:** a ≥0.70 `auto_linked`/`confirmed` identity link from the applicant's net (applicant + directors + PSCs) to a `property:*` node at the matched address (ENR-11 gate).

If any gate fails, no candidate is emitted. Passing candidates are written to the `land_flip_candidates` **review queue** with `buyer_identity_confirmed: false` and a MEDIUM severity ceiling — never to `graph_events`/alerts (the `Severity` type structurally has no `High` variant here; T-23-FalseAccusation safety contract). ≥3 sequential parcel purchases at a postcode/street cluster downgrade the candidate to LOW with a stored `suppression_reason`. The pure gate/tier/suppression functions are DB-free and unit-tested without SurrealDB. Review candidates with the `flips` CLI subcommand.

---

## Two-Key Pattern for Companies House

The ingestor and enricher use different `reqwest::Client` configurations for their respective CH integrations:

- **`ChStreamClient` (ingestor):** Uses `.connect_timeout(30s)` and `.tcp_keepalive(60s)` — no overall `.timeout()`. A long-lived NDJSON stream must not have a response timeout, only a connection-establishment timeout.
- **`run_ch_stream` (core, used by enricher adapters):** Same constraint — caller builds the `reqwest::Client` with `connect_timeout` only, not `.timeout()`.

Do not add `.timeout()` to any client used for CH streaming — it will abort the stream after the timeout elapses, even while data is flowing.

---

## Schema Changes

Node and edge types are defined in `crates/schema/src/types/`. Node and edge weight types derive `serde::Serialize`; all except those in `verdict.rs` (`Verdict`, `Coverage`, `Finding`, …) also derive `serde::Deserialize` — the verdict types are deliberately `Serialize`-only, pinned by the `verdict_does_not_implement_deserialize_owned` test. The attribution helper types in `attribution.rs` (`EvidenceTerm`, `SourceAttribution`, `IdNamespace`) derive neither.

When adding a new node type:
1. Add variant to `NodeWeight` enum in `node.rs`
2. Add corresponding SurrealDB `DEFINE TABLE` in the schema SQL
3. Update `bake_csr` in `snapshot.rs` if the new type needs special handling
4. Update motif detectors that scan all nodes

When adding a new edge type:
1. Add variant to `EdgeWeight` enum in `edge.rs`
2. Ensure `insert_edge` in `storage.rs` handles the new type
3. Add the `DEFINE TABLE … TYPE RELATION` to `SCHEMA_DEF` **and** to `MIGRATION_DDL` in
   `crates/cli/src/commands/migrate.rs`, then bump the count its offline unit test asserts
   (currently 12). A relation table that exists in `SCHEMA_DEF` but not in `MIGRATION_DDL` keeps
   its old, unconstrained definition after a migration and nothing reports it.
4. Add the projector mapping (`edge_rel_table` / `edge_data` in `crates/ingestor/src/projector.rs`)
   — a variant the projector does not know about is counted as a skipped edge, not an error.

> **SurrealDB 3.x datetime coercion.** SurrealDB will not coerce a string into a `datetime` field.
> A `serde_json` `CONTENT` write carrying an RFC3339 *string* into a `TYPE datetime` field, awaited
> with a bare `.await?` (no `.take()` / `.check()`), fails **silently** — the query "succeeds" while
> staging zero rows. The PPD ingest reported **"2 795 151 written"** against a table that held
> nothing.
>
> The fix has three parts, and all three are required:
> 1. **Bind a chrono `DateTime<Utc>`**, not a string — or let SurrealQL produce the value itself
>    with `time::now()`.
> 2. **Write with all-`SET`**, not `CONTENT $row`. `CONTENT` hands SurrealDB a whole serde_json
>    object to coerce field-by-field; `SET` binds each field explicitly.
> 3. **Consume the response with `.take(0)?`** and check it returned a row (Standing Rule 1).
>
> ```rust
> // crates/ingestor/src/backfill/anomaly_sink.rs — the canonical shape
> let written: Vec<serde_json::Value> = session
>     .query("UPSERT type::record($tb, $key) SET kind = $kind, /* … */ detected_at = time::now()")
>     .await?
>     .take(0)?;
> if written.is_empty() {
>     anyhow::bail!("wrote no row — the write this table exists to record was lost");
> }
> ```
>
> Do **not** look for prior art in `crates/core/src/persistence/` for
> "existing date workarounds" — that layer stores `timestamp` as a `u64` of epoch milliseconds
> and never encounters the coercion rule.

---

## Dead Letter Log

Failed SurrealDB writes are logged to `data/dead_letters.jsonl` (configurable via `SurrealWriterConfig::dead_letter_path`). This is a JSONL file — each line is a failed `GraphEvent`. The engine continues running if individual writes fail.

```bash
# Inspect failed writes
cat data/dead_letters.jsonl | jq .
```

---

## Tracing & Logging

The engine uses `tracing` with JSON output. Configure verbosity via `RUST_LOG`:

```bash
# Default — info level
RUST_LOG=info cargo run -p urban-integrity-cli -- serve

# Debug for graph coordinator only
RUST_LOG=urban_integrity_core::graph=debug cargo run -p urban-integrity-cli -- serve

# Trace everything (very verbose)
RUST_LOG=trace cargo run -p urban-integrity-cli -- serve
```

Span names follow `crate::module::function` conventions.

---

## Environment Variables and `.env` File

`AppConfig::load()` calls `dotenv::dotenv().ok()` before reading config, so a `.env` file in the project root is loaded automatically. This is the recommended way to supply secrets locally without putting them in `config.toml`.

Copy `.env.example` to `.env` and fill in your values:

```bash
cp .env.example .env
```

`.env.example` (committed) sets `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` to
`root`/`root` and lists the optional keys commented out. `.env` is gitignored. Typical local
additions:

```
URBAN__INGEST__CH_API_KEY=your-ch-streaming-key
URBAN__ENRICHER__CH__API_KEY=your-ch-rest-key
URBAN__ENRICHER__LAND_REGISTRY__API_KEY=your-hmlr-key
```

Note the prefix separator: `AppConfig::load()` builds `Environment::with_prefix("URBAN").separator("__")`, and in config-rs 0.14.1 an unset prefix separator falls back to the separator — so **only the double-underscore form `URBAN__…` is read**. A single-underscore name such as `URBAN_SURREAL__USERNAME` (which older copies of `.env.example` shipped) does not match the prefix and is silently ignored; it is not a legacy alias. Use `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` instead — the form every test in the workspace uses.

See [CONFIGURATION.md](CONFIGURATION.md) for the full environment-variable and `config.toml` reference.

### Config must reach the code that uses it

A config block that nothing reads is worse than a missing one: it reads as documentation of
behaviour that is not happening. `serve` carried three inline literals — `phoenix_window_days: 730`,
`phoenix_min_value: 10_000.0`, `imd_csv_path: None` — which meant the only process that runs
detection ignored the entire `[detector]` block. Phoenix ran a **730-day** window while
`config.toml` and every doc said 180, and IMD weighting was disabled outright, pinning `m_env` to
1.0 on every alert ever emitted — in direct contradiction of the load-bearing rule that `P_c` is
IMD-weighted.

The wiring is now a named function, `detector_config_from()` in `crates/cli/src/lib.rs`, purely so
it can be asserted on. Its fallbacks match `DetectorConfigRaw::default()`, so an absent block
behaves as documented rather than silently widening the window.

Note what that fixed and what it did not. `serve` now *reads* `[detector].imd_csv_path`, but the
file it points at (`data/wimd_2019_wards.csv`) does **not** arrive with a clone — `data/*` is
gitignored. Until you generate one with `scripts/build_deprivation_ward_index.py --nation <nation>`,
`serve` logs `IMD index load failed` and every alert still carries `m_env = 1.0` with a null
`imd_rank`. That is now a stated, logged condition rather than a silent one, but the weighting is
inert either way. One nation per file: the four UK indices are not comparable, and a startup
jurisdiction preflight refuses an index belonging to a different nation from the analysis's wards.

When you add a config field, add the test that proves it reaches the consumer. "It is in
`config.toml`" is not evidence that anything reads it.

---

## Dependency Notes

- **`petgraph` (workspace-pinned `0.6`)** — CSR storage uses `u32` as `NodeIndex`. Do not mix with `petgraph::graph::NodeIndex` (which wraps `usize`). The codebase uses `CsrNodeIndex = u32` as a type alias where disambiguation is needed.
- **`parking_lot::RwLock`** — used in graph shards (faster than `std::sync::RwLock`)
- **`arc_swap::ArcSwap`** — used for the `SnapshotHandle` (lock-free snapshot rotation)
- **`dashmap`** — concurrent hash map for node/edge maps; shard count configured via `graph.shard_count`
- **`surrealdb` (workspace-pinned `=3.1.0-beta.2`)** — exact-pinned in `[workspace.dependencies]`. Do not bump without a coordinated migration — the schema relies on beta semantics. Server 3.0.x + SDK 3.1.x have a flatbuffers codec mismatch on relation records, worked around via the raw `/sql` HTTP bypass (see CQRS Projector above). The SDK also fails to deserialize very large result sets (a ~55k-row response decodes; ~443k does not) — page full-table scans, see [Reads too large to decode](#reads-too-large-to-decode).
- **No fuzzy-matching crates** — `strsim` and `rphonetic` were removed on 2026-06-13 (commit `43675f8`) when the in-crate fuzzy resolver was deleted. The enricher joins on exact identifiers only; external name screening is delegated to the OpenSanctions/yente sidecar.
- **`urban-integrity-stats` is an empty stub** — `crates/stats/src/lib.rs` is still the untouched `cargo new` template: `pub fn add(left: u64, right: u64)` and its `it_works` test, 14 lines total. `crates/api/Cargo.toml` and `crates/cli/Cargo.toml` both declare a path dependency on it and **no source file in the workspace references it**. Do not go looking there for scoring code: `P_c` and the deprivation index live in `crates/detector/src/scoring/` (`formula.rs`, `imd.rs`, `registry.rs`). Do not add code there either without wiring a consumer to it — an unreferenced crate reads as implemented behaviour that is not happening.
- **`urban-integrity-ingestor` in `crates/enricher`** — present since commit `d3a4ec7`, intended to be limited to `address::{extract_house_number, normalise_postcode}` (PPD adapter + land-flip detector). It no longer is: `land_flip.rs` also imports `address::planning::parse_planning_address`, making three import sites across `adapters/land_registry_ppd.rs` and `land_flip.rs` (verified 2026-08-25 — the count is the thing to re-check, not the line numbers). Do not add a fourth; CH stream primitives come from `urban_integrity_core::stream`.

---

## CLI Subcommands Reference

The `urban-integrity-cli` binary (`crates/cli/src/commands/`) exposes **twenty-three subcommands**: `report`, `alerts`, `anomalies`, `status`, `entity`, `graph-path`, `serve`, `ingest-planning`, `backfill`, `project`, `export-graphml`, `verify`, `enrich`, `links`, `flips`, `migrate`, `purge-orphans`, `purge-ocod-owners`, `analysis`, `fixture`, `probe`, `probe-oracle`, and `workbench`. Every subcommand also accepts the global `--analysis <db_name>` selector (see the Analysis Lifecycle section) — with one live exception documented under `export-graphml` below. The sections below cover subcommands not addressed elsewhere in this guide.

`anomalies` (`commands/anomalies.rs`) reads the `data_anomalies` table — inputs the pipeline
refused to guess at, kept deliberately separate from the alert queue so data-quality noise never
enters an analyst's corruption queue. Subcommands: `list` (`--kind`, `--all`), `show <id>`,
`review <id>` (`--dismiss`). Severity ordering is done in Rust, not SurrealQL — `severity` is a
string column, so a lexical `ORDER BY` would sort `high` after `low`. See the README for the full
surface. Producers of anomaly rows go through `record_anomaly` in
`crates/ingestor/src/backfill/anomaly_sink.rs`; use `DropCounter` there rather than writing one row
per dropped input, so a systematically broken query cannot bury its own finding under volume.

### `project` — Project WAL into native tables

Read the `graph_events` WAL and project it into typed native SurrealDB tables. This is the CQRS projection step — it wipes all native node and relation tables and re-derives them from the WAL, so the output is always consistent with the event log.

```bash
# Project graph_events into native SurrealDB tables
cargo run -p urban-integrity-cli -- project
```

Key behaviours:
- Wipes the 11 native node tables (core + OSINT) after the node stream has loaded, and the 12 relation tables after the edge stream has loaded — never both up front (see CQRS Projector § Design).
- Deduplicates `NodeAdded` events via the shared `fold_latest_node_rows`, keeping the highest-timestamp row so correction events win.
- Loads `EdgeAdded` events via raw HTTP `/sql` to bypass the SurrealDB 3.0.x / 3.1.x SDK flatbuffers codec mismatch on relation records.
- Relates edges in batches of 200 statements per HTTP round-trip.
- Prints a summary: nodes upserted, edges related, nodes skipped, edges skipped (with unknown-src / unknown-dst breakdown); **fails loud** if ≥90% of edges were skipped (see the CQRS Projector section).
- Stamps `projection_runs:current` — `started_at_ms` at the top of the run, `finished_at_ms` only after the final flush. **This is the only way to tell a partial rebuild from a complete one**, because the wipe-then-rebuild design leaves an interrupted run looking exactly like a finished small graph. Check it before trusting the native tables; see [CQRS Projector § Run marker](#run-marker-partial-vs-complete).
- Runs for **well over an hour** on the live corpus (2 h 20 m for ~499k edges on 2026-08-15) and starves concurrent reads. Not a command to fire off mid-session.
- `--dry-run` is accepted but currently prints a message and exits without projecting (placeholder — full dry-run not yet implemented).
- Requires SurrealDB to be running at the configured endpoint (`ws://localhost:8000` by default).

### `export-graphml` — Export graph to GraphML format

Export the HIN graph to a GraphML file for use in external tools (Gephi, yEd, NetworkX, etc.). Builds a `petgraph::graph::DiGraph` and writes a standards-compliant GraphML file with `<key>` declarations for all dynamic node and edge attributes. Implemented in `crates/cli/src/commands/export_graphml.rs`.

Two graph sources via `--source`, and **the default changed to `events` on 2026-08-25** (commit `722b568`):

- **`events`** (default) — reconstructs the graph directly from `NodeAdded` + `EdgeAdded` events in the `graph_events` WAL, over the raw HTTP `/sql` endpoint. No `project` step required.
- **`native`** — reads the six projected node tables and five relation tables through the SDK. Requires `urban-integrity-cli project` to have been run first.

`native` was the default until that commit. It goes through the SurrealDB SDK, whose flatbuffers codec fails to decode typed `RecordId` fields on large result sets — measured 2026-08-25, a 358k-node analysis errors with *Failed to deserialize flatbuffers payload*. It is kept for small graphs and for exporting exactly what `project` produced, but the default should be the path that works at real scale.

```bash
# Export from the WAL (default) to graph.graphml
cargo run -p urban-integrity-cli -- export-graphml

# Read the projected native tables instead (requires a prior `project` run)
cargo run -p urban-integrity-cli -- export-graphml --source native

# Export to a custom path
cargo run -p urban-integrity-cli -- export-graphml --output /tmp/my-graph.graphml

# Slice by type (repeatable, case-insensitive) and drop what filtering left unconnected
cargo run -p urban-integrity-cli -- export-graphml \
  --node-type Officer --node-type Entity --edge-type Directs --drop-isolated

# Shell wrapper: build + export from the WAL, timestamped output
scripts/export_graphml.sh
```

> **`--source events` ignores `--analysis`.** `load_from_events` builds its HTTP request from
> `config.surreal.namespace` / `config.surreal.database` directly and never consults
> `config.analysis`, so a `--analysis an_foo_deadbeef export-graphml` exports the **config.toml
> default database**, not the selected analysis — silently, with a normal-looking success line.
> Only the `native` branch routes through `connect_db` → `resolve_analysis_db`. To export a
> specific analysis from the WAL today, set `URBAN__SURREAL__DATABASE` to its db name (or pass
> `--source native` against a projected graph) and check the row counts against
> `analysis list` before trusting the file.

Key behaviours:
- `--node-type` / `--edge-type` are repeatable and case-insensitive; edges whose endpoints were filtered out are dropped with them. A value matching nothing present is a **hard error listing what is actually there** — never a silent empty export.
- `--drop-isolated` removes nodes left with no edges after filtering. Without it, a type-filtered export is mostly unconnected dots in a layout tool. (`scripts/prune_isolated_nodes.py` does the same to an existing GraphML file.)
- `--source native`: node tables `entity`, `officer`, `location`, `contract`, `ward`, `political_actor`; edge tables `directs`, `located_at`, `awarded_to`, `issued_by`, `represents`. The OSINT tables are not included.
- Edge query for the native source uses `SELECT in.entity_id AS src, out.entity_id AS dst, data FROM {table}` to resolve endpoint `entity_id` values via SurrealDB graph traversal.
- Dynamic node and edge attributes (beyond the fixed `label`, `node_type`, `edge_type` keys) are collected in a single pass and declared as `<key>` elements before the `<graph>` block, so the output is valid GraphML.
- All string values are XML-escaped via `xml_esc()`.
- Edges whose `src` or `dst` does not resolve to a known node are silently skipped; the count is printed at the end.
- `--source native` against empty native tables fails early with a message pointing at `project` or `--source events`.
- Requires SurrealDB to be running at the configured endpoint.

### `backfill ch` — Enrich nodes with Companies House numbers

Link planning applicant `Entity` nodes to their canonical Companies House records by querying the CH REST API. Writes `company_number` back into the existing `NodeAdded` event in `graph_events` (updates `payload.weight_json` in-place).

```bash
# Preview without writing
cargo run -p urban-integrity-cli -- backfill ch --dry-run

# Run enrichment (uses URBAN__INGEST__CH_API_KEY from config.toml or env)
cargo run -p urban-integrity-cli -- backfill ch

# Throttle to 3 API requests per second (default: 2)
cargo run -p urban-integrity-cli -- backfill ch --rate-limit 3
```

Key behaviours:
- Requires `ch_api_key` to be set in `config.toml` or via the `URBAN__INGEST__CH_API_KEY` environment variable; exits with an error if absent.
- Scans all `Entity`-typed `NodeAdded` events in `graph_events` via the paged `scan_events_by_type` helper (the former single-shot `LIMIT 50000` read 6,033 of 33,046 rows — see Query Shapes above).
- `--rate-limit` controls CH REST API requests per second (default: 2; CH enforces ~600/5 min — 2/s. The default was 5 until a live run on 2026-07-28 hit `CH API 429` errors within the first minute).
- `--restart` discards the saved resume cursor and walks every candidate again. A pass is ~20,000 lookups at 2/s — several hours — so an interrupted run resumes by default; use `--restart` only when the candidate set has changed underneath a stale cursor, or to re-test names that previously found no match.
- `--dry-run` performs all lookups and prints matches without writing to `graph_events`.
- Prints a summary: scanned, matched, skipped, errors.

### `backfill ward` — Resolve contract ward codes

Resolve ward codes for `Contract` nodes that have a postcode but no `ward_code` by querying the [postcodes.io](https://postcodes.io) API. Writes the resolved `ward_code` back into the existing `NodeAdded` event in `graph_events`.

```bash
# Preview without writing
cargo run -p urban-integrity-cli -- backfill ward --dry-run

# Run ward code resolution
cargo run -p urban-integrity-cli -- backfill ward
```

Key behaviours:
- Scans all `Contract`-typed `NodeAdded` events that have a postcode but no `ward_code`.
- Uses the postcodes.io bulk lookup endpoint to resolve ward codes without an API key.
- Considers every postcode of every buyer/supplier, not an arbitrary first one: a live
  resolution from any postcode wins, then a terminated-postcode resolution, then skip.
- Postcodes the bulk route returns `null` for are retried through `/terminated_postcodes/{pc}`
  and its centroid reverse-geocoded to today's ward. Edges written that way carry
  `details = "backfill:<role>_ward:terminated_postcode"` so the provenance is visible on the
  edge. On the Anglesey analysis (2026-09-06) this recovered 24 of the 31 postcodes the bulk
  route rejected; the 7 left are a Gibraltar code and typos, which 404 on both routes.
- `--dry-run` performs all lookups and prints matches without writing to `graph_events`.
- Prints a summary: scanned, matched, skipped, errors, plus a `postcode resolution`
  log line with the live/terminated/unresolved split.

**Run it after every Contracts Finder ingest.** The CF mapper never attributes wards — only
this backfill and the planning mapper write `Represents` edges — so each ingest pass leaves its
new `CF:` contracts unattributed until this is re-run. The `ward-concentration` probe's
"N contracts lack ward attribution" remediation item is exactly that residue: on 2026-09-06 it
read 533, of which 461 were contracts ingested on 2026-08-18, the day after the previous (only)
backfill pass. One run brought it to 14, all with postcodes no route can resolve or no parties.
Note the IMD caveat: the wards this resolves are overwhelmingly English `E05…` codes, and the
Anglesey analysis loads the Welsh WIMD index, so closing the gap fixes attribution and the
probe's count but does not change `m_env` for those contracts.

### `backfill planning` — Repair planning applicant attribution

Repair planning `Contract` nodes whose applicant attribution is unresolved, and stamp explicit
`applicant_provenance` across the planning corpus. Implemented in
`crates/ingestor/src/backfill/planning.rs`.

```bash
cargo run -p urban-integrity-cli -- backfill planning --dry-run
cargo run -p urban-integrity-cli -- backfill planning
```

Two independent passes, deliberately kept apart:

1. **Edge-scoped re-scrape.** Eligibility is decided by each Contract's *current* `AwardedTo` edge
   target — only Contracts still pointing at the `director:unknown-applicant` placeholder qualify
   (~2,005 live). It is **not** decided by `applicant_provenance.is_none()`: because `graph_events`
   is append-only, that naive gate would match the full ~39,860-row corpus and insert ~75,000
   duplicate `AwardedTo` / `LocatedAt` edges for records that were already correctly attributed.
   Eligible Contracts are re-listed ward by ward (only wards that actually hold a
   placeholder-attributed Contract), and newly-resolved applicants get a **new** edge pair.
2. **Bulk stamp.** Planning Contracts outside the edge-scoped set get provenance stamped in place.

The old placeholder edges are never mutated and never deleted — edges are temporal.

### `backfill cessation` — Anchor Phoenix on real dissolution dates

Fetch real `date_of_cessation` values from Companies House so Phoenix's window is anchored where a
company actually failed rather than where the graph first observed it. Flags: `--dry-run`,
`--rate-limit` (default 2/s), `--restart`, `--max-fetches N`.

A dry run answers only from the existing profile cache — uncached targets are reported as skipped
rather than fetched — and it neither reads nor writes the resume cursor, since it changes nothing
and must not claim progress. `--max-fetches` bounds a first live pass against the CH quota. See
[ARCHITECTURE.md § `backfill cessation`](ARCHITECTURE.md#backfill-cessation--a-suspicion-not-a-verdict)
for why a cessation date is treated as a suspicion rather than a verdict.

### `backfill officers` — Fetch officers, keyed on CH identity evidence

Fetch officers for Companies House entities that have none. This is the first-class replacement for
`scripts/ch_backfill.py --officers-for-ch-entities`: it is resumable and keys on the identity
evidence CH publishes (officer id, appointment date, role) rather than on the name alone. Flags:
`--dry-run`, `--rate-limit`, `--restart`, `--max-fetches N`, plus two data-correction modes:

- `--audit` — check the `director:` nodes **already** in the graph against CH evidence and record an
  `officer_key_collision` anomaly for each key that conflates people CH says are different, flagging
  separately the ones already fused (those assert a false identity today). The write guard cannot see
  what earlier tools wrote, and the Python script's name-only output is still in the graph.
- `--split-collisions` (conflicts with `--audit`) — move officer events off the uncorroborated
  colliding keys the audit reports, re-keying each on the CH officer id its company (or appointment
  date and role) attributes it to. Events no CH evidence attributes are left exactly where they are
  and reported, never guessed at. This **rewrites `graph_events`**, so run `--dry-run` first and
  `project` afterwards.

### `backfill psc-links` — Re-run the PSC↔officer join without CH calls

Re-runs the PSC↔officer identity join over data **already in the graph** — no Companies House API
calls. The join normally runs inside the `ch_psc` adapter's fetch loop, so correcting the matcher
would otherwise mean re-fetching every PSC record to re-derive links from data already staged.

```bash
cargo run -p urban-integrity-cli -- backfill psc-links --dry-run   # count only
cargo run -p urban-integrity-cli -- backfill psc-links
```

Idempotent: existing `ch_psc` links are skipped, not duplicated.

### Backfill durability — `DbSession` and resume cursors

Every backfill is a long-running, single-connection job — `backfill ch` walks ~20,000 Companies
House lookups at 2 requests/second, so a full pass is multiple hours, and SurrealDB's HTTP session
token expires after roughly one hour. Measured live 2026-07-28: a `backfill ch` run died at the ~1h
mark with `401 Unauthorized for url (http://localhost:8000/rpc)` having linked 354 of 20,660
candidates. `serve` already handled this; the backfill path did not.

- **`crates/ingestor/src/backfill/session.rs`** — `DbSession` is the single choke point every
  backfill query goes through. It re-signs in and retries once when a query is rejected for an auth
  reason, and runs a periodic proactive refresh so the reactive path stays a backstop. The database
  it records is the one `--analysis` **resolved** to, not `config.surreal.database`: signing in
  again resets the connection's namespace/database selection, so reading the raw config field would
  move a mid-flight run onto a different analysis.
- **`crates/ingestor/src/backfill/cursor.rs`** — resume cursors for backfills whose cost is
  *external*. The ward pass is naturally re-entrant (a stamped Contract drops out of the candidate
  set), but `backfill ch` is not: its two negative verdicts (`NoMatch`, `Ambiguous`) leave the
  entity unchanged, and 20,306 of 20,660 candidates ended in one of those, so a restart re-issues
  20,306 CH lookups — roughly three hours of API budget spent re-learning nothing. The cursor stores
  a position and running totals and nothing that could be mistaken for graph data; deleting a cursor
  row costs repeated work, never correctness.

### `entity` — Company lookup

Look up a company by name (case-insensitive substring match), slug (exact ID), or Companies House number.

```bash
# Case-insensitive substring name search (table output)
cargo run -p urban-integrity-cli -- entity "Apex Build Ltd"

# Exact slug lookup
cargo run -p urban-integrity-cli -- entity apex-build-ltd

# CH number lookup (exact)
cargo run -p urban-integrity-cli -- entity --ch-number 12345678

# JSON output, pipe to jq
cargo run -p urban-integrity-cli -- entity "Apex Build Ltd" --output json \
  | jq '.directors[].name'

# Disambiguation: multiple substring matches return a summary list
cargo run -p urban-integrity-cli -- entity "Alpha" --output json
```

Key behaviours:
- `--ch-number` and a positional `query` are mutually exclusive.
- CH numbers are validated at the CLI boundary (2–10 alphanumeric characters) before any DB call.
- If the slug form is supplied and no exact match is found, the command falls back to a name search (D-01 dual-mode).
- When a name search returns multiple matches, the output is a disambiguation table (table mode) or a `EntityProfileSummary[]` array (JSON mode). Use `--ch-number` for the exact record.
- `--output md` and `--output pdf` are not supported by this command — use `report` instead.
- In table mode a **LPA Presence** section is rendered showing the count of contracts and planning applications per Local Planning Authority. In JSON mode `lpa_presence` is included as a top-level array alongside the base profile fields (each entry carries `lpaCode`, `contractCount`, and `planningApplicationCount`).

### `ingest-planning` — On-demand planning portal scraping

Trigger an immediate scrape of one or all configured planning portals and write the resulting graph events to SurrealDB.

```bash
# Scrape a single ward
cargo run -p urban-integrity-cli -- ingest-planning --ward X05999001

# Scrape all configured portals
cargo run -p urban-integrity-cli -- ingest-planning --all
```

Key behaviours:
- Requires SurrealDB to be running — the command reads existing keys for dedup before writing.
- If the Scrapling sidecar is unreachable, the command prints a warning and the Firecrawl fallback is attempted per portal (see the Scrapling Sidecar section below).
- Results are printed as a summary table showing ward code, applications found, events written, and events skipped (already in DB).
- Planning portals are configured via `[[ingest.planning_portals]]` entries in `config.toml`. Each entry requires `ward_code`, `url`, and `portal_type`.

### `graph-path` — Shortest path between two entities

Find the shortest path between any two entities in the HIN graph using BFS traversal (up to 6 hops). Implemented in `crates/cli/src/commands/graph_path.rs`.

```bash
# Trace a company to a councillor (table output)
cargo run -p urban-integrity-cli -- graph-path "Apex Build Ltd" "John Smith"

# Use slugs for exact disambiguation
cargo run -p urban-integrity-cli -- graph-path GB-COH:12345678 director:john-smith

# JSON output — returns nodes[] and edges[] arrays
cargo run -p urban-integrity-cli -- graph-path "Apex Build Ltd" "John Smith" --output json
```

Key behaviours:
- Both endpoints are resolved via the canonical `resolve_entity` seam (supports name, slug, or CH number).
- If either endpoint resolves to multiple candidates the command lists them and exits — re-run with a slug or CH number.
- Path traversal is undirected (edges are traversed in both directions) and capped at 6 hops (`MAX_HOPS`).
- Table output renders the path as `Name → EDGETYPE → Name → ...`. JSON output returns `{nodes: [{id, name}], edges: [{type}]}`.
- `--output md` and `--output pdf` are not supported — use `report` instead.

### `enrich` — Run OSINT enrichment adapters

Run one or all registered OSINT enrichment adapters against the current graph. Adapters query external data sources, join on exact identifiers, score candidate identity links, and write results to the `identity_links` table. Implemented in `crates/cli/src/commands/enrich.rs`.

```bash
# Run all enabled adapters
cargo run -p urban-integrity-cli -- enrich

# Run a specific adapter by name
cargo run -p urban-integrity-cli -- enrich --source ch_psc
cargo run -p urban-integrity-cli -- enrich --source ch_disqualified
cargo run -p urban-integrity-cli -- enrich --source land_registry_ccod
cargo run -p urban-integrity-cli -- enrich --source land_registry_ocod
cargo run -p urban-integrity-cli -- enrich --source london_gazette
cargo run -p urban-integrity-cli -- enrich --source charity_commission
cargo run -p urban-integrity-cli -- enrich --source opensanctions
cargo run -p urban-integrity-cli -- enrich --source icij
cargo run -p urban-integrity-cli -- enrich --source electoral_commission
cargo run -p urban-integrity-cli -- enrich --source council_registers
cargo run -p urban-integrity-cli -- enrich --source opencorporates
cargo run -p urban-integrity-cli -- enrich --source ppd
cargo run -p urban-integrity-cli -- enrich --source planning_identity
cargo run -p urban-integrity-cli -- enrich --source mutuals_register

# Run ONLY the land-flip post-detector (pseudo-source, not an adapter)
cargo run -p urban-integrity-cli -- enrich --source land-flip
```

Key behaviours:
- `--source all` (default) runs all adapters whose `is_enabled()` returns true. Disabled adapters (missing credentials or licence gates) are skipped silently.
- `--source <name>` runs a single adapter. If no adapter is registered under that name a warning is logged and empty stats are returned (no error).
- `--source land-flip` dispatches the Pre-Permission Land Flip **detector** directly (FLIP-02) — it writes `land_flip_candidates` and emits no graph mutations.
- Per-adapter failures are logged as warnings; other adapters continue running.
- After all adapters complete, six post-adapter detectors run: Disqualified Director Evasion (D-10), Property Accumulation Anomaly (PROP-03), Offshore Shadow Company (FUZ-05), Charity-Commercial Crossover (FUZ-06), Political Donation → Contract Correlation (POL-04), and Pre-Permission Land Flip (FLIP-02).
- Graph mutations are drained to the `graph_events` WAL by a dedicated drain task (`wal_writer::drain_to_wal`); the CLI path has no hot graph (D-01). Any WAL write failure propagates as a non-zero exit (fail-fast, D-04).
- A background task re-authenticates **both** SurrealDB sessions (the engine's and the drain task's — `Surreal::clone()` sessions have independent auth) every 50 minutes, against the **resolved analysis database**, so `--analysis` runs longer than the token TTL neither lose writes to a 401 nor silently switch back to `main`.
- Prints a summary: candidates found, auto-linked, pending review.
- Requires SurrealDB to be running at the configured endpoint.
- Background enrichment (sleep-first interval loop) is wired in the `serve` context. The CLI `enrich` command is for on-demand runs.

### `links` — Review identity link candidates

Manage `identity_links` records produced by the enricher. Supports four sub-operations. Implemented in `crates/cli/src/commands/links.rs`.

```bash
# List all pending_review links, ordered by confidence DESC (uncapped)
cargo run -p urban-integrity-cli -- links list

# Show evidence detail for a specific link
cargo run -p urban-integrity-cli -- links show identity_links:abc123

# Confirm a link — updates status to 'confirmed' and writes a graph_events WAL row
cargo run -p urban-integrity-cli -- links confirm identity_links:abc123

# Reject a link — suppresses re-proposal in future enrichment runs
cargo run -p urban-integrity-cli -- links reject identity_links:abc123
```

Key behaviours:
- All `id` arguments reach SurrealDB only via `.bind()` — never string interpolation (T-16-08).
- `links list` reads every `pending_review` record via `scan::scan_identity_links` (no cap) and sorts them by `confidence` descending in Rust.
- `links confirm` updates `status = 'confirmed'` (via `UPDATE type::record($table, $key) SET status = $status`, the SurrealDB 3.x RecordID form), prints the D-08 confirmation line `Confirmed: {source} -> {target} ({resolution_type}). Hot graph updated.`, and then writes an `EdgeConfirmed` row to the `graph_events` WAL (D-09) via `urban_integrity_enricher::wal_writer::write_wal_event`. Both endpoint values come from the server-side SELECT and are sanitised by `build_wal_record_id`.
- `links reject` updates `status = 'rejected'` (same RecordID UPDATE form), suppressing re-proposal in future enrichment runs (REV-04).
- `links show` pretty-prints `evidence_json` if it is valid JSON.
- There is no `--force` or `--auto-confirm` flag (REV-05 / T-16-09 constraint).
- Requires SurrealDB to be running at the configured endpoint.

### `flips` — Review land-flip candidates

Manage the `land_flip_candidates` review queue produced by the land-flip detector (FLIP-04; D-02/D-03). Implemented in `crates/cli/src/commands/flips.rs`.

```bash
# List pending candidates (review queue, newest first)
cargo run -p urban-integrity-cli -- flips list

# Show all three gate-evidence sections for one candidate
cargo run -p urban-integrity-cli -- flips show land_flip_candidates:abc123

# Confirm a candidate as a genuine land-flip (optional analyst note)
cargo run -p urban-integrity-cli -- flips confirm land_flip_candidates:abc123 --note "matches HMLR record"

# Dismiss a candidate and suppress re-proposal
cargo run -p urban-integrity-cli -- flips dismiss land_flip_candidates:abc123 --note "unrelated purchaser"
```

Key behaviours:
- `confirm`/`dismiss` update status (plus optional `--note` and `reviewed_at`) and re-SELECT to verify a row was actually hit.
- Neither operation touches the hot graph or writes any WAL/`graph_events` row — candidates are review-queue records, not graph edges (T-23-HotGraph).
- SurrealDB 3.x `id` fields are RecordIDs, not strings, so lookups use `type::record($table, $key)` — a `WHERE id = $id` string binding never matches.

### `migrate` — Apply relation-table IN/OUT constraints

Apply IN/OUT schema constraints to every `TYPE RELATION` table and re-project (SCH-01..SCH-04). Implemented in `crates/cli/src/commands/migrate.rs`.

> **The DDL covers 12 relation tables; one help string still says 11.** `MIGRATION_DDL` defines
> `directs`, `located_at`, `awarded_to`, `issued_by`, `represents`, `controls`, `trustee_of`,
> `owns_property`, `offshore_link`, `donates_to`, `disqualified_from` and `declares_interest` — and
> the offline unit test asserts exactly 12 `TYPE RELATION FROM` clauses. The `Commands::Migrate`
> help string in `main.rs` still says "11 relation tables"; the `println!` lines in `migrate.rs`
> print a count derived from the DDL. The DDL is what runs; the help text is stale, not a missing table.

```bash
# Preview the DDL without executing
cargo run -p urban-integrity-cli -- migrate --dry-run

# Apply: REMOVE + redefine every relation table, then re-project
cargo run -p urban-integrity-cli -- migrate
```

Key behaviours:
- **Prerequisite:** run `urban-integrity-cli enrich` first so OSINT nodes are present in `graph_events` — missing OSINT nodes cause an `edges_skipped` regression after `REMOVE TABLE` wipes the relation data.
- The migration DDL removes and redefines all 12 `TYPE RELATION` tables; the DEFINE statements deliberately use no `IF NOT EXISTS` (SCH-02: remove first, redefine cleanly) and no `ENFORCED` keyword (D-14). These structural properties are verified by an offline unit test.

### `purge-orphans` — Remove orphan Property records

Purge orphan `Property` nodes and their `identity_links` left by the pre-fix CCOD ingest crash. Implemented in `crates/cli/src/commands/purge.rs`.

```bash
# Dry run (default): count and report what would be deleted, delete nothing
cargo run -p urban-integrity-cli -- purge-orphans

# Delete — prompts for the run-derived confirmation digest, requires a TTY
cargo run -p urban-integrity-cli -- purge-orphans --apply
```

Key behaviours:
- An orphan is a property node that **nothing owns** — i.e. its id never appears as the destination of an `OwnsProperty` edge. This definition is ownership-edge based and format-independent, so it is correct for both ownership models: CCOD (owner is a `GB-COH:` UK company) and OCOD (owner is a `GB-ROE:`/`GB-OCOD:` OffshoreEntity). Overseas-owned OCOD records are **not** orphans.
- Also scans `identity_links` (`source=land_registry`) whose owner `target_node_id` is untracked.
- Full-table scans are paged because the SurrealDB 3.0.x server / 3.1.0-beta.2 SDK pairing fails to deserialize very large result sets — see [Reads too large to decode](#reads-too-large-to-decode).
- The orphan predicate is pure (no I/O) and unit-tested without a DB.

### The four controls on the delete path

`--apply` is not a single flag any more. Four independent controls sit between the scan and the
delete, and each exists because a specific failure already happened:

1. **A typed confirmation digest.** The command prints a 12-hex-char SHA-256 over *this run's*
   sorted, de-duplicated orphan ids and requires the operator to type it back. Because the token
   is derived from the scan output, it cannot be pre-baked into a script, an alias, or shell
   history — which is exactly how the surrealkv OOM fix got bypassed five times in one day
   (2026-06-17). Comparison is exact and case-sensitive; only surrounding whitespace is trimmed.
2. **A hard refusal when stdin is not a terminal.** A non-TTY returns `Err`, never `Ok(false)`, so
   a caller cannot confuse "no confirmation possible" with "confirmation declined". CI, cron and
   scripts are categorically unable to reach the delete path. **There is no `--yes`, no `--force`
   and no env-var escape anywhere in this file. Do not add one.**
3. **The real removed-count, checked hard.** Batches use the locked
   `DELETE type::table($table) WHERE id IN $ids RETURN BEFORE` syntax, so the response carries the
   rows actually removed. `delete_in_batches` counts `removed.len()` — **never** `chunk.len()`,
   the size of the batch sent — and any mismatch is an `anyhow::bail!`, never a `warn!`. That is
   Standing Rule 1 applied to a delete instead of a write, and it is what caught the
   backtick-quoting bug (Standing Rule 8): `sent 10000 keys, SurrealDB removed 0`, aborted with
   nothing deleted. Batch size is 10,000.
4. **A blast-radius cap.** `purge-orphans --apply` refuses outright when the orphan set exceeds
   **1%** of the scanned property population. The pre-existing guard only refused an *empty*
   owned-set, so it would not have caught the historical 93,052-record OCOD misclassification —
   that owned set was non-empty, just built from the wrong definition. The live baseline is 0
   orphans of 105,809 scanned (`an_anglesey_aebaa785`, 2026-08-21/22), so 1% leaves ample headroom
   while still catching a misclassification anywhere near the historical bug's scale (~87% of the
   scanned population, two orders of magnitude past the cap).

The scan is the read-only first pass and the delete works from the exact ids it produced — there
is deliberately no post-confirmation re-scan, so the rows deleted are always the rows the digest
committed to. Aborting and re-running simply repeats the whole flow with a fresh scan and a fresh
digest.

### `purge-ocod-owners` — Retire superseded OCOD owner events

Retire superseded `GB-OCOD:`-keyed owner nodes and their ownership edges from `graph_events`
(INT-03/D-14), also in `crates/cli/src/commands/purge.rs`.

```bash
# Dry run (default): count only
cargo run -p urban-integrity-cli -- purge-ocod-owners

# Delete — same typed-digest + TTY gate as purge-orphans
cargo run -p urban-integrity-cli -- purge-ocod-owners --apply
```

Key behaviours:
- `graph_events` is **append-only**, so these superseded events must be retired *before*
  `enrich --source land_registry_ocod` re-emits owners under the corrected key. Skip the purge and
  the re-run adds correctly-keyed nodes *beside* the old fragmented ones instead of merging them.
- `GB-ROE:`-keyed owners are never in scope and are never counted.
- It reuses `confirmation_digest`, `confirm_interactively` and `delete_in_batches` directly rather
  than shipping a second implementation, so controls 1–3 above apply unchanged.
- **Control 4 (the blast-radius cap) is deliberately not applied here.** For `purge-orphans` an
  implausibly large share signals a misclassified definition; here, selecting 100% of the
  `GB-OCOD:`-sourced rows is the correct and intended scope, so a ratio cap would fire on the
  success case. The controls that do apply are the typed digest, the operator's out-of-band
  backup, and the fail-loud removed-count check.
- Sequence the whole cycle deliberately: purge → `enrich --source land_registry_ocod` → `project`.
  The 2026-08-22 run aborted between the purge and the re-emit (Standing Rule 9) and left the
  analysis with no OCOD ownership until the re-emit succeeded.

### `analysis` — Provision and inspect analyses

See the **Analysis Lifecycle** section above for `analysis create` / `analysis list` / `analysis remove` and the global `--analysis` selector.

### `probe` — Run the forensic battery

Run the 22-probe battery against `graph_events`, print a verdict table, and write
`evidence-report.md`. Select the target with the global `--analysis`.

```bash
cargo run -p urban-integrity-cli -- probe
cargo run -p urban-integrity-cli -- --analysis <db_name> probe
cargo run -p urban-integrity-cli -- probe --output reports/evidence.md --json findings.json
cargo run -p urban-integrity-cli -- probe --full-battery
```

| Flag | Effect |
|------|--------|
| `--output <path>` | Where to write the Markdown report. Defaults to `evidence-report.md` in the current directory. |
| `--json <path>` | Additionally write the raw `Vec<Finding>` battery as JSON. Independent of `--output`. |
| `--full-battery` | Dispatch every probe unconditionally (`run_probes`) instead of the gated path. Gating is the default; this flag exists so a gated run can always be compared against the complete one. |

Key behaviours:
- **Gating is the default.** `execute()` calls `dispatch_battery`, which validates the probe
  dependency graph and then runs `graph::run_gated`. A probe is skipped when a **declared**
  dependency did not return `Hit`/`Weak` (an AND across its declared edges), and every Tier-2 probe
  is additionally skipped when **no** Tier-1 probe returned `Hit`/`Weak` (an OR across the tier —
  gate-time logic read from `Probe::tier()`, deliberately not modelled as graph edges). Today the
  graph carries exactly one real declared edge: `address-cluster-exact` depends on
  `identity-integrity`.
- **A skipped probe still produces a `Finding`**, carrying `Verdict::Skipped` and the cause that
  gated it. The cause is constructed where the real upstream `Finding` is in scope, so a report can
  never name a cause that did not actually gate the probe. No population provenance is computed for
  a skipped probe — it never ran, so there is nothing to attribute.
- **The dependency graph is validated on both paths.** `dispatch_battery` validates before it
  branches, so `--full-battery` does not skip the acyclicity, unknown-dependency-name or
  tier-interleaving checks.
- **Dispatch order is topological; output order is registration order.** `run_gated` iterates
  `graph.dispatch_order()` internally but fills a slot array indexed by each probe's original
  position. `ensure_built_for` refuses to dispatch a graph against a slice it was not built for,
  comparing names positionally rather than just lengths — a same-length substituted slice would
  otherwise under-gate *silently*, because `depends_on()` answers `&[]` for a name the graph does
  not know.
- Verdicts are `HIT` / `WEAK` / `CLEAR` / `NOT_APPLICABLE` (each carrying `population` and `capable`
  counts), `DATA_GAP` / `BLOCKED` (which carry none, because nothing was measured), or `SKIPPED`
  (gated out). A probe that hits an internal error returns `DATA_GAP` rather than failing the
  command.
- Opens **two** connections — the analysis DB and `_catalog`, the latter needed only by
  `identity-integrity` for its coverage baseline — both derived from a single `AppConfig::load()`
  inside one `execute()` call, so the two can never resolve onto different analyses. Population
  provenance is then computed on that *same* connection, never a second `connect_db()`.
- Both artifacts are attempted before either failure is reported, so an unwritable `--json` path
  does not also cost you the Markdown report the battery has already earned. Both create their
  parent directory if it is missing.
- `scripts/analyst_probe.py` is **retained as the differential oracle** (PRB-07, 2026-08-31), not
  superseded — see its entry in the Python script reference.
- Requires SurrealDB to be running at the configured endpoint.

See [Adding a Tier-1 Probe](#adding-a-tier-1-probe) for the constraints on writing one.

### `verify` — Pre-signature counterparty audit

Audit a company before contract signature. Checks for disqualified directors, active evasion alerts, beneficial ownership, and existing motif detections.

```bash
# Audit by CH number (required)
cargo run -p urban-integrity-cli -- verify --company-number 12345678
```

Key behaviours:
- `--company-number` is required and validated at the CLI boundary (2–10 alphanumeric characters).
- Checks `identity_links` for `ch_disqualified` source records matching any officer of the company.
- Checks `graph_events` for `MotifDetected` events with `motif_type='disqualified_director_evasion'` linked to the company's officers (two-hop check).
- PSC beneficial ownership surfaced from `identity_links WHERE source='ch_psc'`.
- Exit code `0` = low/medium risk; exit code `1` = critical/high risk (active evasion or confirmed disqualification).
- Implemented in `crates/cli/src/commands/verify.rs`. Requires SurrealDB running — `serve` does not need to be active.

---

## Analyst Workbench (wasm frontend)

The workbench is a browser UI served by the CLI's `workbench` subcommand. It spans three crates
with a hard dependency boundary between them:

| Crate | Target | Role |
|-------|--------|------|
| `urban-integrity-workbench-dto` | native **and** `wasm32-unknown-unknown` | serde-only view types; the only workspace crate the browser may depend on for data |
| `urban-integrity-workbench` | native | Axum handlers, replay-only boot, graph algorithms, in-process probe runs, `rust-embed` asset serving |
| `urban-integrity-workbench-web` | `wasm32-unknown-unknown` | Leptos 0.8 CSR frontend (`publish = false`) |

`workbench-web` must **never** gain a dependency on `urban-integrity-schema`, `-core`,
`-detector`, `surrealdb`, `tokio`, `petgraph` or `rayon`; `scripts/check-wasm-boundary.sh` enforces
that in CI. Native-only algorithm crates (`graphrs`, `arrow`) belong in `crates/workbench`.

### Toolchain

- The `wasm32-unknown-unknown` target comes from `rust-toolchain.toml`; rustup installs it
  automatically.
- **Trunk 0.21.14** is the bundler: `cargo install --locked trunk --version 0.21.14`.
- **Check which `trunk` is on your `PATH`.** `pg-trunk` (a PostgreSQL extension manager) also
  installs a binary called `trunk`, and its `trunk build` does something entirely unrelated.
  `scripts/build-workbench.sh` refuses to run when `trunk --version` starts with `pg-trunk`. If you
  need both, install the bundler somewhere else and put it first on `PATH` for the build, rather
  than running a bare `cargo install trunk` that can clobber the other one.
- Node is only needed to regenerate the vendored Cosmograph shim (see below), not for a normal
  build.

### Build and re-embed

Trunk is a separate, explicit build step — never a `build.rs` side effect — so `cargo build
--workspace` and `cargo test --workspace` keep working on a machine with no Trunk, no Node and no
wasm target.

```bash
# 1. Bundle the frontend. Trunk.toml sets dist = "../workbench/dist", so the output lands
#    directly in crates/workbench/dist/ (gitignored) where rust-embed reads it.
scripts/build-workbench.sh

# 2. Rebuild the binary that embeds it. Required after every frontend build — see below.
touch crates/workbench/src/lib.rs && cargo build -p urban-integrity-cli
```

`crates/workbench/src/assets.rs` embeds `dist/` with `rust-embed`, using the **`debug-embed`**
feature so assets are baked in at compile time in *every* profile, including `cargo run` debug
builds — the server never reads `dist/` from disk at request time. The consequence: **a new
`dist/` is invisible until the embedding crate is recompiled.** Cargo does not track `dist/` as
an input, so touching `crates/workbench/src/lib.rs` is what forces the re-embed. Skip it and the
workbench serves the previous frontend with no warning.

If `dist/` has never been built, `#[allow_missing = true]` compiles an empty asset set and the
server returns a "Workbench assets not built" page instead of failing.

### Running it

```bash
# Seed, project and probe the planted fixture analysis (idempotent; re-seeds on re-run)
scripts/toy-analysis.sh

# Serve it — bind address from [workbench] bind_addr in config.toml (127.0.0.1:8090),
# overridable with --bind or URBAN__WORKBENCH__BIND_ADDR
cargo run -p urban-integrity-cli -- workbench --analysis <printed slug>
cargo run -p urban-integrity-cli -- workbench --bind 127.0.0.1:8091 --open
```

`workbench` binds its listener **before** doing any expensive work, so a port already in use is
an immediate error. A leftover server from an earlier session is the usual cause — check with
`ss -ltnp | grep 8090`. Killing a `cargo run` parent can orphan the child process that actually
holds the port, so find and stop the `urban-integrity-cli workbench` process itself.

`scripts/toy-analysis.sh` drives `analysis create --fixture`, `fixture seed --planted --reseed`,
`project` and `probe` against a synthetic, content-addressed analysis whose expected answers are
literals in `crates/test-support/src/planted.rs`. Use it for UI work and manual checks rather than
the live corpus, and **re-run it before a manual check** — a stale fixture database looks like a
fixture that does not exercise the probes. The script is several cargo invocations; do not run it
alongside another build.

Before any visual check, confirm the server is serving the `dist/` you just built (compare the
served wasm file's hash with the one in `crates/workbench/dist/`).

### The Cosmograph shim

`crates/workbench-web/js/cosmograph-shim.js` is a **vendored** ESM bundle loaded through
`#[wasm_bindgen(module = …)]`; its source, `crates/workbench-web/js-build/cosmograph-shim-src.js`,
is never consumed at cargo build time. Change the source, then regenerate the bundle and commit
both:

```bash
cd crates/workbench-web/js-build
npm ci
npm run build   # esbuild → ../js/cosmograph-shim.js
```

CI's `scripts/check-shim-drift.sh` rebuilds from source and fails if the two have diverged —
editing the bundle by hand is exactly the drift it catches.

### Tests

`crates/workbench-web/tests/` are native tests (routing/render and *reachability* sweeps such as
`abbr_reachable.rs`, which fail if a component exists but is wired into no page).
`crates/workbench/tests/` covers the server APIs against the planted fixture
(`planted_golden.rs`, `entity_api.rs`, `population_equals_export.rs`, …), and
`crates/cli/tests/workbench_boot.rs` / `toy_analysis_chain.rs` exercise the CLI end to end.

### Stored reports must stay deserialisable

Probe results are persisted for the workbench (`crates/workbench/src/report_persistence.rs`).
Adding a required field (no `#[serde(default)]`) to a **persisted** DTO makes every previously
stored report undeserialisable, and the workbench then boots as if no probe had ever run. Request
types may stay strict; stored types must default new fields, with a test against a fixture
serialised before the change.

---

## Scrapling Sidecar Development

The `scrapling-sidecar/` directory is a standalone Python FastAPI process — it is **not** part of the Cargo workspace. The Rust engine communicates with it via HTTP on `localhost:8765`.

### Setup

```bash
cd scrapling-sidecar
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

Dependencies (from `requirements.txt`): `fastapi>=0.115.0`, `uvicorn[standard]>=0.30.0`, `scrapling>=0.3.0`, `firecrawl-py>=1.0.0`, `pydantic>=2.0.0`. `playwright>=1.44.0` is listed commented-out — only needed for JS-rendered portals, which none of the live strategies require.

### Running

```bash
# Default port 8765
python sidecar.py

# Custom port
SIDECAR_PORT=9000 python sidecar.py
```

The sidecar degrades gracefully: if it is down when the Rust engine starts, a `WARN` log is emitted and ingestion continues without planning portal data.

### Wire protocol

```
POST /scrape
  Request:  { "url": str, "selectors": {}, "timeout": int, "portal_type": str }
  Response: { "status": "ok"|"error", "results": [...], "reason": str|null }

GET /health
  Response: { "status": "ok", "service": "scrapling-sidecar" }
```

Each result object contains: `reference`, `description`, `address`, `ward`, `applicant` (all nullable strings except `reference`).

### Supported portal types

| `portal_type` | Strategy | Status |
|--------------|----------|--------|
| `"idox"` | Idox PublicAccess — two-step GET form → POST ward via `scrapling.fetchers.Fetcher` | Live |
| `"swift_apas"` | SwiftLG / Swift APAS portals (Dudley, Walsall) — search POST + detail-page GET | Live |
| `"northgate"` | Northgate PlanningExplorer (Birmingham — POST-form .aspx, needs browser session state) | Stub — TBD |
| `"arcus"` | Arcus Global / Salesforce Experience Cloud registers — no browser: GET the register page for the Aura bootstrap ids, POST the guest search action scoped to the config ward, then batch-fetch each record's `getRecord` detail (the list projection omits applicant, agent and received date) | Live |
| `"wfs"` | WFS data source (e.g. Scotland Spatial Hub) | Stub — TBD |
| `"planapi"` | PlanAPI commercial aggregator | Stub — TBD |
| _any other_ | Generic CSS selector fallback (`scrape_static_css`) | Live |

`portal_type` is set in `config.toml` per portal entry and is the authoritative routing key — URL pattern matching is not used, so routing survives council URL restructuring.

> **`follow_redirects` must be passed per request, never on the constructor.**
> `Fetcher(follow_redirects=False)` is *accepted and then ignored* — Scrapling logs
> "deprecated … have no effect" and follows redirects anyway. This is the sidecar's SSRF control:
> with the constructor form, a validated public host that 302s to `127.0.0.1:8000` bypasses the URL
> guard entirely and the scraper fetches SurrealDB. Every `.get()` / `.post()` call site in
> `sidecar.py` therefore passes `follow_redirects=False` explicitly. Keep it that way in any new
> scraper.

> **"Could not ask" and "asked, got nothing" are different outcomes.** A scraper that could not
> reach the portal raises `ScrapeFailed`; `[]` means the portal answered and published nothing.
> The Rust caller relies on that distinction to decide between leaving applicant provenance
> untouched and stamping `fetch_failed`. Collapsing the two into an empty list turns a measurement
> failure into a false negative.

### Adding a new portal system

1. Add a scraper function: `scrape_{system}(url, selectors, timeout) -> list[dict]`
2. Add an `elif strategy == "{system}":` branch in the `/scrape` endpoint dispatch in `sidecar.py`
3. Set `portal_type = "{system}"` in the relevant `[[ingest.planning_portals]]` entries in `config.toml`

No Rust changes are needed — the `portal_type` string passes through `crates/ingestor/src/planning/sidecar.rs` unchanged.

### Running sidecar tests

```bash
cd scrapling-sidecar
source .venv/bin/activate
pytest
```

Tests are in `tests/test_sidecar.py` and `tests/test_arcus.py`, and use fixture HTML/JSON (no network calls). The `pytest.ini` sets `pythonpath = .` so `sidecar` imports resolve without installation.

---

## Firecrawl Fallback Integration

The Idox scraping strategy in `sidecar.py` has a three-tier fallback cascade:

1. **Primary — Scrapling** (`scrapling.fetchers.Fetcher`): two-step GET form → POST ward with CSRF token extraction. Fetches detail pages for ward and applicant fields (1-second sleep between requests for ethical rate limiting).
2. **Fallback tier 1 — Firecrawl stealth GET** (`_firecrawl_get`): calls `firecrawl.scrape_url(url, formats=["html"])` and parses the returned HTML with `_parse_idox_html` using the same CSS selectors as the primary path. Lower credit cost than tier 2.
3. **Fallback tier 2 — Firecrawl browser actions** (`_firecrawl_idox_actions`): replays the Idox ward-search form interaction via Firecrawl's `actions` API (wait → write ward → click submit → wait → scrape). Used for portals that require genuine session state.

Tier 1 and 2 are skipped silently if `FIRECRAWL_API_KEY` is not set in the environment — the sidecar logs a warning and returns an empty result list rather than failing.

### Enabling Firecrawl

Set the API key before starting the sidecar:

```bash
export FIRECRAWL_API_KEY=fc-your-key-here
python sidecar.py
```

A 402 response from the Firecrawl API (credits exhausted) is caught and logged as a warning; it does not crash the sidecar.

### `_parse_idox_html` helper

`_parse_idox_html(html: str, url: str) -> list[dict]` parses a pre-fetched Idox results page HTML string using `scrapling.Selector`. It applies the same CSS selectors (`li.searchresult`, `p.metaInfo`, `.summaryLinkTextClamp`, `p.address`) as the primary scraper but requires no network call. Both fallback tiers reuse this helper.
