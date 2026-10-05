<!-- generated-by: gsd-doc-writer -->
# Testing Guide

<!-- GSD-MANAGED-DOC: testing -->

## Test Suite Overview

The workspace is **13 crates** (`schema`, `detector`, `core`, `ingestor`, `stats`, `api`, `cli`,
`enricher`, `probe`, `test-support`, `workbench-dto`, `workbench`, `workbench-web` — the `members`
list in the root `Cargo.toml`). Tests use the standard Rust harness only: `#[test]` and
`#[tokio::test]` functions, run by `cargo test`. There is no separate test framework, no
`nextest` configuration, and no browser or DOM runner — the Leptos frontend is tested host-side
(see *Analyst Workbench Tests*).

Two different numbers describe the suite, and they should never be quoted as if they were the same
thing:

- **A static census of the tree** — test functions counted in the source. On `main` at `d828be7f`
  (2026-09-24) that is **2,784** `#[test]`/`#[tokio::test]` functions, **423** of them DB-gated.
- **A measured run** — what `cargo test` actually printed. The latest recorded whole-workspace run
  is **2,573 passed, 0 failed, 11 ignored** (gated, 2026-09-16, **637s wall**). It was 1,709 passed
  / 10 ignored on 2026-09-03, 1,655 on 2026-09-01 and 1,424 on 2026-08-25.

The two will not reconcile exactly: the census is taken at a later commit, doctests appear in a run
but not in the census, and the census counts function definitions rather than harness output. Both
drift with every merge. Re-measure rather than quoting either:

```bash
cargo test --workspace 2>&1 | grep -E '^test result' | awk '{p+=$4; f+=$6; i+=$8} END {print p, f, i}'
```

Static census by crate (`main` at `d828be7f`, 2026-09-24):

| Crate | Test functions | DB-gated |
|-------|---------------:|---------:|
| `workbench-web` | 550 | 0 |
| `enricher` | 449 | 69 |
| `core` | 398 | 91 |
| `cli` | 322 | 75 |
| `ingestor` | 321 | 28 |
| `probe` | 239 | 127 |
| `workbench` | 235 | 30 |
| `workbench-dto` | 87 | 0 |
| `schema` | 77 | 0 |
| `detector` | 66 | 0 |
| `api` | 26 | 2 |
| `test-support` | 13 | 1 |
| `stats` | 1 | 0 |
| **Total** | **2,784** | **423** |

`stats` is still the `cargo new` template — its one test is the template's `it_works`. The frontend
crate `workbench-web` now holds the largest block of tests in the workspace, and none of them needs
a database.

| Tier | Command | Adds | Requires |
|------|---------|------|----------|
| **Base** | `cargo test --workspace` | unit tests + DB-free integration tests, including every `workbench-web` host-side test and the source-scanning guards | nothing — no external services |
| **DB integration** | `URBAN_DB_TESTS=1 cargo test --workspace` | **423** SurrealDB-backed tests (static census, `d828be7f`) across `probe`, `core`, `cli`, `enricher`, `workbench`, `ingestor`, `api` and `test-support` | SurrealDB on `:8000`, launched via `scripts/surreal-start.sh` **from the repo root**; the 2 clone tests in `analysis_lifecycle.rs` also need the `surreal` binary |
| **Live external** | `URBAN_LIVE_TESTS=1 cargo test ...` | 8 external-API tests (Companies House REST, Contracts Finder OCDS), all in `crates/ingestor/tests/live.rs` behind the `live_only!()` macro | a Companies House REST API key + internet; **not run in CI** |

A fourth, narrower gate — `URBAN_PLANNING_TESTS=1` — enables the 2 planning-portal scrape tests in
`crates/ingestor/tests/planning_integration.rs`, which need the Scrapling sidecar on
`localhost:8765`.

The workspace builds **87 integration-test binaries** — one per file under `crates/*/tests/*.rs`:
20 in `core`, 18 in `cli`, 12 in `workbench`, 11 in `enricher`, 10 each in `ingestor` and
`workbench-web`, 4 in `probe`, and 1 each in `api` and `detector`. Each crate's library (and each
`src/main.rs`/`src/bin` binary) adds a unit-test target on top. Do not read the number of
`test result:` blocks as a binary count — doctest targets print their own blocks.

### How to count the DB-gated tier (and how not to)

**Attribute each gate expression to its enclosing `#[test]`/`#[tokio::test]` function, then count
the functions.** Three things make a naive count wrong:

- **Grepping for gate *sites* over-counts.** The helper *definitions* themselves match:
  `db_tests_enabled()` is defined in `crates/test-support/src/lib.rs` and **re-defined locally in
  many test files**, `skip_if_no_db()` in `crates/cli/tests/cli.rs`, and every one of those
  definitions contains the literal `URBAN_DB_TESTS`. Doc-comments and module headers that merely
  mention the variable match too — this document included.
- **Ignoring string literals under-counts.** A sizeable minority of tests gate with a bare
  `std::env::var("URBAN_DB_TESTS")`, where the gate name lives inside a string. A census that
  strips string literals before matching (to keep braces balanced) silently loses all of them.
- **Brace-tracking that is not string-aware over-counts.** A `{` or `}` inside a string literal —
  `"{"` in a `format!` fragment, a SurrealQL literal, a JSON fixture — unbalances a naive body
  scan, which then runs past the end of one test function and swallows the next, merging a gated
  test's body into an ungated neighbour's and tallying both.

Counted correctly — comments stripped, string literals kept but excluded from brace tracking, one
tally per test function whose body reaches `URBAN_DB_TESTS`, `db_tests_enabled()` or
`skip_if_no_db()` — the census on `main` at `d828be7f` (2026-09-24) is **423**. It was 332 at
`52f3fb8` (2026-09-03), 326 at `10dfed2` (2026-09-01) and 215 at `722b568` (2026-08-25). Most of the
growth since 2026-09-03 is the probe battery gaining DB-gated tests in its new crate, the entity
profile read path in `core`, the workbench server's router tests, and the planted-fixture goldens.

Per-file census, sorted by DB-gated count:

| File | DB-gated | of total tests in file |
|------|---------:|-----------------------:|
| `crates/core/tests/surreal_integration.rs` | 34 | 35 |
| `crates/cli/tests/cli.rs` | 21 | 44 |
| `crates/cli/tests/analysis_lifecycle.rs` | 17 | 17 |
| `crates/enricher/tests/engine_coverage.rs` | 13 | 13 |
| `crates/probe/tests/planted_evidence_refs.rs` | 10 | 10 |
| `crates/probe/src/electoral_commission_donations.rs` | 9 | 9 |
| `crates/core/src/persistence/scan.rs` | 8 | 16 |
| `crates/core/tests/saved_investigations.rs` | 8 | 8 |
| `crates/enricher/src/land_flip.rs` | 8 | 32 |
| `crates/enricher/tests/psc_adapter.rs` | 8 | 22 |
| `crates/ingestor/tests/cursor_integration.rs` | 8 | 8 |
| `crates/probe/src/osint_new_node_types.rs` | 8 | 8 |
| `crates/workbench/tests/entity_api.rs` | 8 | 12 |
| `crates/core/tests/oracle_shapes.rs` | 7 | 7 |
| `crates/probe/src/identity_link_registry.rs` | 7 | 7 |
| `crates/probe/src/land_flip_candidates.rs` | 7 | 7 |
| `crates/probe/src/precomputed_alerts.rs` | 7 | 11 |
| `crates/probe/src/property_nodes.rs` | 7 | 7 |
| `crates/probe/src/psc_beneficial_ownership.rs` | 7 | 7 |
| `crates/enricher/tests/disqualified_adapter.rs` | 6 | 22 |
| `crates/enricher/tests/evasion_detector.rs` | 6 | 6 |
| `crates/probe/src/detector_readiness.rs` | 6 | 10 |
| `crates/probe/src/disqualified_active_in_procurement.rs` | 6 | 6 |
| `crates/probe/src/evasion_alerts.rs` | 6 | 7 |
| `crates/cli/tests/evidence_report_defects.rs` | 5 | 5 |
| `crates/core/src/persistence/land_flip_runs.rs` | 5 | 5 |
| `crates/enricher/src/adapters/ch_disqualified.rs` | 5 | 24 |
| `crates/enricher/src/adapters/ch_psc.rs` | 5 | 40 |
| `crates/enricher/tests/property_accumulation.rs` | 5 | 5 |
| `crates/ingestor/src/backfill/planning.rs` | 5 | 37 |
| `crates/probe/src/disqualification_chain.rs` | 5 | 6 |
| `crates/probe/src/donation_contract_correlation.rs` | 5 | 5 |
| `crates/probe/src/opencorporates_cross_jurisdiction.rs` | 5 | 6 |
| `crates/probe/src/shared_exact_address.rs` | 5 | 7 |
| `crates/probe/src/temporal_burst.rs` | 5 | 5 |
| `crates/workbench/tests/planted_profile.rs` | 5 | 5 |
| `crates/cli/src/commands/purge.rs` | 4 | 22 |
| `crates/cli/tests/evidence_report_structure.rs` | 4 | 5 |
| `crates/cli/tests/probe_tier2_e2e.rs` | 4 | 7 |
| `crates/core/src/persistence/land_flip_capability.rs` | 4 | 4 |
| `crates/core/tests/profile_as_at.rs` | 4 | 10 |
| `crates/core/tests/profile_provenance.rs` | 4 | 11 |
| `crates/probe/src/gazette_insolvency_signals.rs` | 4 | 4 |
| `crates/probe/src/geographic_concentration.rs` | 4 | 6 |
| `crates/probe/src/osint_confirmed_intel.rs` | 4 | 4 |
| `crates/cli/tests/workbench_boot.rs` | 3 | 3 |
| `crates/core/tests/entity_query_status_fold.rs` | 3 | 3 |
| `crates/core/tests/profile_paging.rs` | 3 | 3 |
| `crates/core/tests/profile_pivot_ids.rs` | 3 | 3 |
| `crates/enricher/tests/donation_correlation_endpoint.rs` | 3 | 3 |
| `crates/ingestor/src/backfill/mod.rs` | 3 | 3 |
| `crates/probe/src/analyst_review_queue.rs` | 3 | 3 |
| `crates/probe/src/identity_integrity.rs` | 3 | 3 |
| `crates/probe/src/ward_concentration.rs` | 3 | 3 |
| `crates/workbench/src/alerts/mod.rs` | 3 | 14 |
| `crates/workbench/tests/report_persistence.rs` | 3 | 3 |
| `crates/api/tests/server.rs` | 2 | 16 |
| `crates/cli/src/commands/migrate.rs` | 2 | 4 |
| `crates/cli/tests/evidence_report_contradictions.rs` | 2 | 11 |
| `crates/cli/tests/motif_first_seen_last_seen.rs` | 2 | 2 |
| `crates/cli/tests/probe_oracle_agreement.rs` | 2 | 2 |
| `crates/cli/tests/probe_tier1_e2e.rs` | 2 | 3 |
| `crates/core/tests/detector_run_marker.rs` | 2 | 2 |
| `crates/core/tests/findings_by_entity.rs` | 2 | 12 |
| `crates/core/tests/replay_status_transition.rs` | 2 | 2 |
| `crates/enricher/src/adapters/council_registers.rs` | 2 | 9 |
| `crates/enricher/src/wal_writer.rs` | 2 | 5 |
| `crates/enricher/tests/opencorporates_targets.rs` | 2 | 2 |
| `crates/ingestor/src/backfill/cursor.rs` | 2 | 8 |
| `crates/ingestor/tests/projector_ocod_owner_merge.rs` | 2 | 2 |
| `crates/ingestor/tests/projector_run_marker.rs` | 2 | 2 |
| `crates/workbench/src/boot.rs` | 2 | 12 |
| `crates/workbench/tests/canvas_filter_api.rs` | 2 | 3 |
| `crates/workbench/tests/investigation_api.rs` | 2 | 2 |
| `crates/workbench/tests/report_view_equals_finding_list.rs` | 2 | 2 |
| `crates/cli/src/commands/export_graphml.rs` | 1 | 31 |
| `crates/cli/src/commands/flips.rs` | 1 | 4 |
| `crates/cli/src/commands/links.rs` | 1 | 1 |
| `crates/cli/tests/evidence_report_pivotability.rs` | 1 | 2 |
| `crates/cli/tests/population_source_corroboration.rs` | 1 | 1 |
| `crates/cli/tests/purge_delete_spike.rs` | 1 | 1 |
| `crates/cli/tests/toy_analysis_chain.rs` | 1 | 1 |
| `crates/core/tests/contract_row_fields.rs` | 1 | 3 |
| `crates/core/tests/profile_sections.rs` | 1 | 4 |
| `crates/enricher/src/adapters/land_registry_ocod.rs` | 1 | 14 |
| `crates/enricher/src/adapters/land_registry_ppd.rs` | 1 | 14 |
| `crates/enricher/tests/ccod_adapter.rs` | 1 | 16 |
| `crates/enricher/tests/gazette_adapter.rs` | 1 | 12 |
| `crates/ingestor/src/backfill/session.rs` | 1 | 5 |
| `crates/ingestor/tests/planning_integration.rs` | 1 | 10 |
| `crates/ingestor/tests/projector_declares_interest.rs` | 1 | 1 |
| `crates/ingestor/tests/projector_disqualification.rs` | 1 | 1 |
| `crates/ingestor/tests/projector_donates_to.rs` | 1 | 1 |
| `crates/ingestor/tests/projector_property_prefix.rs` | 1 | 1 |
| `crates/probe/src/attributed.rs` | 1 | 4 |
| `crates/test-support/src/lib.rs` | 1 | 3 |
| `crates/workbench/tests/degree_distribution.rs` | 1 | 2 |
| `crates/workbench/tests/finding_star_golden.rs` | 1 | 1 |
| `crates/workbench/tests/population_equals_export.rs` | 1 | 1 |
| **Total** | **423** | |

Neighbouring numbers are all reachable from the same tree with a slightly different rule, so say
which rule you used. Historically, blanking string literals gave 297 rather than 326 at `10dfed2`,
and re-introducing the string-unaware brace bug gave 333 rather than 332 at `52f3fb8`. The
`crates/test-support/src/lib.rs` row is the boundary case worth naming on its own:
`dropping_a_test_ns_removes_the_namespace` is a DB-gated test *of the harness itself*, not of the
product. Count it or don't, but say which.

### Measured figures

The most recent recorded measurements (2026-09-16, against a store holding **986,062
`graph_events`**, proved first by the three store checks under *A fast gated run is the thing to
distrust*):

| Gate | Result |
|------|--------|
| `URBAN_DB_TESTS=1 cargo test --workspace` | **2,573 passed, 0 failed, 11 ignored**, **637s wall** |
| `cargo test -p urban-integrity-cli --test cli` | **44 passed** in **0.13s** (its DB-gated tests returning at the gate) |
| `URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test cli` | the *identical* **44 passed** in **440.11s** |

The gated and ungated summary lines for the CLI binary differ **only** in the trailing duration.
That is not a coincidence — see the next section.

`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` are CI gates
(the `lint` job) and are expected to be clean on `main`.

**Ignored tests.** The static census finds **8 `#[ignore]`-attributed tests** plus **3 doctests
marked `ignore`**; the 2026-09-16 run reported 11 ignored. Re-measure before quoting either. The
attributed ones:

```
rep_02_default_filename_format                               crates/cli/tests/report_md.rs — duplicate coverage
rep_02_output_file_writes_to_disk                            crates/cli/tests/report_md.rs — filesystem write
rep_05_entity_scoped_report_resolves_by_name                 crates/cli/tests/report_md.rs — needs SurrealDB
rep_03_missing_pandoc_writes_md_then_errors                  crates/cli/tests/report_pdf.rs — PATH mutation
backfill::cessation::live_cache_tests::the_real_python_cache_parses_without_dropping_a_line
backfill::officers::live_officer_cache_tests::the_real_officer_cache_keys_every_collision_on_evidence
regenerate_population_arrow_fixture                          crates/workbench/tests/regenerate_population_arrow.rs — writes a fixture into workbench-web
test_graphql_with_live_data_from_contracts_finder            crates/api/tests/server.rs — needs live internet (Contracts Finder)

crates/core/src/persistence/surreal.rs — persistence::surreal::surreal_writer_loop (line 140)   [doctest]
crates/schema/src/ch_number.rs — ch_number::normalise_ch_number (line 57)                       [doctest]
crates/test-support/src/lib.rs — (line 15)                                                      [doctest]
```

The two `live_cache_tests` are the operator's local-cache tests — they read
`data/ch-company-status-cache.jsonl` and `data/ch-officers-cf-awardees.jsonl`, both under the
gitignored `data/`, and their `#[ignore]` reason strings say so. Run them with `--ignored` only on a
machine that has run the backfills that produce those caches; past the `#[ignore]` they panic on a
missing file rather than skipping. See *The second silent skip* near the end of this document.

`regenerate_population_arrow_fixture` is not a check at all: it is the reproduction path for
`crates/workbench-web/src/testdata/population-real-arrow.bin`, the real Arrow IPC payload the
frontend's parser test reads. It is `#[ignore]`d because it writes into another crate's source
tree. Run it deliberately, against a seeded store:

```bash
URBAN_DB_TESTS=1 cargo test -p urban-integrity-workbench --test regenerate_population_arrow -- --ignored
```

### The DB gate is a skip-by-return, so the pass count proves nothing

**This is the single most important thing in this document.** `URBAN_DB_TESTS` is checked *inside*
each test, which returns early when it is unset:

```rust
if !db_tests_enabled() { return; }
```

A test that returns at the gate still **passes**. `cargo test --workspace` and
`URBAN_DB_TESTS=1 cargo test --workspace` therefore report the *same* number of passing tests, and a
green ungated run says **nothing whatsoever** about whether the 423 DB-gated tests exercised
anything. There is no `#[ignore]`, no "filtered out" count, no skipped tally: the two runs are
indistinguishable in their summary lines.

**Never record "tests pass" for a DB-gated test without `URBAN_DB_TESTS=1` set and a reachable
SurrealDB.** A pass recorded from an ungated run is a record that the test returned at line one.
This project has been bitten by exactly that before: gated tests marked "done" that had never once
executed.

**Verify the gate by wall-clock time instead** — but read the next section before trusting a
duration on its own. Gated against ungated:

| Binary | Tests | Gated (`URBAN_DB_TESTS=1`) | Ungated | Corpus-sensitive? | Measured |
|--------|------:|---------------------------:|--------:|-------------------|----------|
| `crates/cli/tests/cli.rs` | 44 | **440.11s** | **0.13s** | **yes** | 2026-09-16 |
| `crates/core/tests/oracle_shapes.rs` | 6 | **14.11s** | **0.00s** | no | 2026-09-03 |
| `crates/core/tests/surreal_integration.rs` | 35 | **5.59s** | **0.00s** | no | 2026-09-03 |

(The static census now finds 7 test functions in `oracle_shapes.rs`; the timing above was taken when
it held 6.)

The last column matters when deciding what a timing actually proved:

- **`core`'s integration tests prove `URBAN_DB_TESTS=1` reached *a* database.** They build their own
  namespaces through `urban-integrity-test-support` and seed every row they assert on, so they cost
  the same against an empty store as against the live one, stable to within a second either way. A
  non-zero duration here means a connection was made and rows were written. It means nothing about
  *which* store.
- **The CLI pair proves it reached the *right* database.** `tests/cli.rs` spawns the debug-profile
  binary against whatever store the running instance actually serves, with no namespace override
  and no seeded fixture, so its runtime is a function of how much data that store holds.

If a DB-gated binary reports 0.00s *with* `URBAN_DB_TESTS=1`, the gate did not engage — usually
SurrealDB is not listening and the TCP pre-flight skipped everything — and that run proved nothing.

**A `cargo test` filter that matches zero tests also exits 0:**

```bash
cargo test --workspace this_test_name_does_not_exist_anywhere   # "0 passed; 0 failed"; exit 0
```

Every binary reports `0 passed; 0 failed; ... N filtered out` and the command succeeds. A stale
test name in a checklist therefore reads as a pass while proving nothing — the same failure shape as
the DB gate, reached a different way. Check the printed `test <name> ... ok` line, not the exit
code.

**`URBAN_DB_TESTS` was not the only silent skip.** Two tests in `crates/ingestor/src/backfill/`
used to return by the same trick when a fixture file under the gitignored `data/` directory was
absent. They now carry an `#[ignore]`, so the summary line distinguishes the two states — see *The
second silent skip: fixture files that are not in the repository* near the end of this document.

### A fast gated run is the thing to distrust, not a slow one

The failure this section exists to catch is not a gated run that hangs. It is a gated run that
**finishes quickly, passes everything, and proves nothing**, because the instance it talked to was
not the store you meant. The discriminating gap is roughly 250x, not 3x.

On 2026-09-03, `URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test cli` finished in
**1.79s**. Every count matched the expected 44 passed. `cargo clippy --workspace --all-targets --
-D warnings` was clean. **Only the duration dissented.** SurrealDB had been launched by
`scripts/surreal-start.sh` **from inside a git worktree**. The launcher derives its
`WorkingDirectory` from its own location and the store path is relative, so a worktree launch
silently creates a fresh, empty per-worktree RocksDB store with no `urban_integrity` namespace at
all. `/health` returned 200, connections succeeded, the `core` integration tests passed at their
normal gated duration, and the CLI tests ran against an empty corpus and flew.

**Never run `scripts/surreal-start.sh` from a worktree.** `data/` is gitignored, so a worktree
launch cannot find the real store and will not tell you so.

**Before trusting any gated timing, assert the first two checks; add the third when the number
matters.** Duration is the weakest of the signals — never rest on it alone:

```bash
# 1. The service is rooted at the repo, not at a worktree.
systemctl --user show urban-surreal -p WorkingDirectory
#    must print the repo root

# 2. The instance is actually serving the real store.
curl -s -u root:root -H 'Accept: application/json' \
     --data 'INFO FOR ROOT;' http://localhost:8000/sql | grep -o urban_integrity
#    must find the `urban_integrity` namespace

# 3. Count the corpus against the exact namespace and database the CLI tests will use
#    (config.toml's [surreal] namespace and database).
curl -s -u root:root -H 'surreal-ns: urban_integrity' -H 'surreal-db: <db_name>' \
     -X POST --data 'SELECT count() FROM graph_events GROUP ALL;' http://localhost:8000/sql
```

A row count is a fact about the store; a duration is a fact about the store *and* its cache, the
machine's load and whatever else was running. A green `/health` is **not** one of these checks — it
was 200 throughout the incident. Neither is the pass count, and neither is clippy.

Remember that an analysis id is the **database**, not the namespace. Putting it in the namespace
slot silently creates an empty namespace and returns zero rows rather than erroring.

### What the gated run actually costs

**Budget ~10-15 minutes for a full `URBAN_DB_TESTS=1 cargo test --workspace`** against an
already-built workspace (637s wall on 2026-09-16), and **~450s** for the CLI binary on its own.
Treat ~450s as the shape of a healthy CLI run, warm *or* cold. **Do not read a gated run that
finishes in ten minutes as a misconfigured store** — the rule above is about a run that finishes in
*seconds*.

There are eight recorded measurements of `URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli
--test cli`, and they fall into two tight clusters:

| Cluster | Measurements | Dates |
|---------|--------------|-------|
| Slow | 1,935s; 1,406.85s; 1,789.57s | 2026-08-20; 2026-09-03 (two runs, same evening, same 44 tests, 27% apart) |
| Fast | 443.29s; 464.15s; 440.11s; 434.51s; 429.75s | all 2026-09-16, spanning 8.0% |

Corpus size does not explain the gap: the fast runs were against 986,062 `graph_events`, the slow
2026-09-03 runs against 977,302 — slightly *more* data for a third of the time. Earlier revisions of
this document quoted 1,406.85s as the figure; it was wrong by roughly 3x as a description of a
healthy run.

**Cache warmth was tested and ruled out (2026-09-16).** Restarting the `urban-surreal` service drops
RocksDB's block cache and memtables (resident memory fell from 6.43GB to 208MB), and the kernel page
cache over the store's files was also measured cold (about 9% resident). The run that began 28
seconds after that restart finished in **434.51s**; one hour later, **429.75s**; two days warm,
**440.11s**. The spread is 2.4% and does not run in the direction a cache hypothesis predicts. The
test file is byte-identical between the slow and fast runs, and no index on `graph_events` changed
in between.

**What remains is contention on the machine**, with a correlation but not a demonstrated mechanism:
memory-pressure cache flushes logged by the desktop session were frequent on every day a slow
measurement was taken and near-absent on the days of the fast ones. Do not quote that as the cause.
What it justifies is where to look: when a gated run comes in far slower than ~450s, examine what
else the machine is doing before suspecting the database.

`test_cli_report_markdown_creates_file` dominates the CLI binary. It is not a fixture test: it
spawns the debug-profile CLI with `report --format md --output-file test-report-ci-output.md` and
**no `--analysis` selector**, so it runs against whatever the configured default database holds —
on a working machine, the live corpus — and renders the whole report to Markdown. The harness
announces it: `test test_cli_report_markdown_creates_file has been running for over 60 seconds` is
the cheapest signal that you are in this situation.

```rust
// crates/cli/tests/cli.rs — the shape that costs most of the binary's runtime
let output = std::process::Command::new(cli_bin())
    .args(["report", "--format", "md", "--output-file", "test-report-ci-output.md"])
    .current_dir(workspace_root)
    .env("URBAN__SURREAL__WS_URL", "ws://localhost:8000")
    // ... no namespace override, no `--analysis`, no seeded fixture
```

That is a real test-design smell: the test asserts only that the file exists and carries the
expected header, so it does not need the production corpus at all. Rebuilding it on a `TestNs`
namespace seeded with a handful of alerts (or on the planted fixture) would assert exactly as much
in a fraction of a second and stop the assertion depending on machine-local data.

The practical consequence: **do not reach for the full gated workspace run as a quick check.** Run
the binary you changed (`cargo test -p <crate> --test <file>`), and let CI carry the full sweep.

### Build concurrency

**Run one cargo invocation at a time on a developer machine.** A per-invocation `jobs` cap does
nothing about two invocations running together, and rustc on this workspace peaks at roughly 3GB
per process. Overlapping `cargo build`, `cargo clippy` and `cargo test` runs — plus
`scripts/toy-analysis.sh` (several `cargo run`s) or `scripts/build-workbench.sh`
(`trunk build --release`), which count as cargo invocations too — have exhausted a 31GB machine
before. If a long cargo command is moved to the background, wait for it to exit before starting any
other.

### At a glance

Run the **live** tier locally before merging any change that touches the ingestor HTTP clients or
mappers. Those tests are the only thing that catches upstream API schema drift, and CI cannot run
them (no key in CI, third-party availability).

The toolchain is **pinned to `1.96.0`** via `rust-toolchain.toml` (with `clippy` + `rustfmt`
components and the `wasm32-unknown-unknown` target). rustup auto-installs it on the first cargo
invocation in the workspace, locally and on CI runners — so local clippy and CI clippy lint with the
same ruleset, and the frontend's wasm build needs no separate `rustup target add`.

For what the crates under test actually do, see `docs/ARCHITECTURE.md`. For the `URBAN__*`
configuration the tests read, see `docs/CONFIGURATION.md`.

| Level | Tool | Env flag | Location |
|-------|------|----------|----------|
| **Unit** | `cargo test` | — | `#[cfg(test)]` blocks inline |
| **Integration** | `cargo test` | — | `crates/*/tests/` |
| **Shared DB harness** | (library) | — | `crates/test-support/src/lib.rs` — `TestNs`, `NsGuard`, `db_tests_enabled()` |
| **Planted fixture** | (library) | — | `crates/test-support/src/planted.rs` (`planted_graph()`, `Manifest`), `crates/test-support/src/events.rs` (`seed_planted` and other seed helpers) |
| **Query-shape guard** | `cargo test --test query_shape_guard` | — | `crates/core/tests/query_shape_guard.rs` (30 tests, source-level, no DB) |
| **Hybrid Validation** | `cargo test --test hybrid_validation` | — | `crates/core/tests/hybrid_validation.rs` |
| **UpdateNode / CSR** | `cargo test --test coordinator_update_node` | — | `crates/core/tests/coordinator_update_node.rs` |
| **SurrealDB Integration** | `URBAN_DB_TESTS=1 cargo test` | `URBAN_DB_TESTS=1` | `crates/core/tests/surreal_integration.rs`, `oracle_shapes.rs`, `entity_query_status_fold.rs`, `replay_status_transition.rs`, `contract_row_fields.rs`, `detector_run_marker.rs`, `saved_investigations.rs`, `profile_*.rs`, `findings_by_entity.rs`, `crates/ingestor/tests/cursor_integration.rs`, `projector_*.rs` |
| **Probe battery** | `URBAN_DB_TESTS=1 cargo test -p urban-integrity-probe` | `URBAN_DB_TESTS=1` for 127 of 239 | `crates/probe/src/**`, `crates/probe/tests/` (4 binaries), plus `crates/cli/tests/probe_tier1_e2e.rs`, `probe_tier2_e2e.rs`, `probe_oracle_agreement.rs`, `probe_report_statics.rs` |
| **Workbench server** | `cargo test -p urban-integrity-workbench` | `URBAN_DB_TESTS=1` for 30 of 235 | `crates/workbench/src/**`, `crates/workbench/tests/` (12 binaries) |
| **Workbench DTOs** | `cargo test -p urban-integrity-workbench-dto` | — | `crates/workbench-dto/src/**` (87 inline, no DB) |
| **Workbench frontend** | `cargo test -p urban-integrity-workbench-web` | — | `crates/workbench-web/src/**` (inline), `crates/workbench-web/tests/` (10 source-scanning binaries) |
| **Frontend CI guards** | `scripts/check-*.sh` | — | `wasm-build` CI job — see *Continuous Integration* |
| **Enricher DB tests** | `URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher` | `URBAN_DB_TESTS=1` | `crates/enricher/tests/engine_coverage.rs`, `evasion_detector.rs`, `psc_adapter.rs`, `disqualified_adapter.rs`, `ccod_adapter.rs`, `gazette_adapter.rs`, `property_accumulation.rs`, `donation_correlation_endpoint.rs`, `opencorporates_targets.rs` |
| **Live Integration** | `URBAN_LIVE_TESTS=1 cargo test` | `URBAN_LIVE_TESTS=1` | `crates/ingestor/tests/live.rs` |
| **Planning sidecar** | `URBAN_PLANNING_TESTS=1 cargo test --test planning_integration` | `URBAN_PLANNING_TESTS=1` | `crates/ingestor/tests/planning_integration.rs` |
| **CLI DB tests** | `URBAN_DB_TESTS=1 cargo test` | `URBAN_DB_TESTS=1` | `crates/cli/tests/cli.rs`, `toy_analysis_chain.rs`, `workbench_boot.rs`, `motif_first_seen_last_seen.rs` |
| **Purge (orphans / OCOD owners)** | `URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli purge` | `URBAN_DB_TESTS=1` for 4 of 22 | `crates/cli/src/commands/purge.rs`, `crates/cli/tests/purge_delete_spike.rs` |
| **Analysis lifecycle** | `URBAN_DB_TESTS=1 cargo test --test analysis_lifecycle` | `URBAN_DB_TESTS=1` (+ `surreal` binary for the 2 clone tests) | `crates/cli/tests/analysis_lifecycle.rs` |
| **Status-transition regression** | `cargo test --test upsert_status_transition` | — (replay half: `URBAN_DB_TESTS=1`) | `crates/core/tests/upsert_status_transition.rs`, `crates/core/tests/replay_status_transition.rs` |
| **Enricher tests** | `cargo test -p urban-integrity-enricher` | — | `crates/enricher/tests/hot_path.rs`, `jurisdiction.rs`, `crates/enricher/src/confidence.rs` |
| **Micro-benchmarks** | `cargo bench` | — | `crates/detector/benches/motif_detectors.rs` |
| **Macro-benchmarks** | `cargo bench` | — | `crates/core/benches/macro_bench.rs`, `concurrent_readers.rs` |

---


## Running Tests

```bash
# Full workspace — base suite (no external services)
cargo test --workspace

# Single crate
cargo test -p urban-integrity-core
cargo test -p urban-integrity-detector
cargo test -p urban-integrity-probe
cargo test -p urban-integrity-workbench
cargo test -p urban-integrity-workbench-web      # host-side, no wasm, no DB

# Single integration test binary
cargo test --test hybrid_validation         # crates/core/tests/
cargo test --test coordinator_update_node -p urban-integrity-core  # UpdateNode / CSR
cargo test --test integration -p urban-integrity-detector  # crates/detector/tests/
cargo test -p urban-integrity-workbench --test planted_golden      # algorithms vs planted manifest, no DB
cargo test -p urban-integrity-workbench-web --test abbr_reachable  # one source-scanning guard

# Full suite including SurrealDB integration tests.
# Requires: SurrealDB running on :8000 — start it with the launcher (isolated, memory-capped,
# RocksDB backend). NEVER `surreal start` directly; a guard reroutes a bare invocation.
# NEVER run the launcher from a git worktree: it derives WorkingDirectory from its own location,
# `data/` is gitignored, and you get a silent empty store that passes everything in seconds.
#   scripts/surreal-start.sh          # from the repo root
# Budget ~10-15 minutes (637s wall on 2026-09-16; see "What the gated run actually costs").
URBAN_DB_TESTS=1 cargo test --workspace

# Before trusting any gated timing, prove the instance is serving the real store:
systemctl --user show urban-surreal -p WorkingDirectory     # must be the repo root
curl -s -u root:root -H 'Accept: application/json' \
     --data 'INFO FOR ROOT;' http://localhost:8000/sql | grep -o urban_integrity

# The DB-gated binary you actually changed — seconds, not minutes. Prefer this locally.
URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test surreal_integration
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test engine_coverage
URBAN_DB_TESTS=1 cargo test -p urban-integrity-probe --test planted_evidence_refs
URBAN_DB_TESTS=1 cargo test -p urban-integrity-workbench --test entity_api

# Probe battery (crates/probe) and its CLI-side end-to-end / static-scan binaries
cargo test -p urban-integrity-probe --lib graph                      # 18 tests, no DB
cargo test -p urban-integrity-cli --test probe_report_statics        # static scan, no DB
URBAN_DB_TESTS=1 cargo test -p urban-integrity-probe                 # the whole battery, gated
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test probe_tier1_e2e
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test probe_oracle_agreement

# The toy analysis end to end: provision, seed, probe, boot the workbench, compare to the manifest
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test toy_analysis_chain

# Purge / OCOD owner-merge suites (see "OCOD Owner Merge and the Purge Path" below)
cargo test -p urban-integrity-cli purge
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli purge
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_ocod_owner_merge
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher icij_name_index_pages_and_returns_every_row

# A filter that matches nothing still exits 0 — check the printed `test <name> ... ok` line,
# never the exit code, when you are citing a specific test as evidence.
cargo test --workspace this_test_name_does_not_exist_anywhere   # "0 passed; 0 failed"; exit 0

# Analysis lifecycle tests (DB-gated; the 2 clone tests also need the `surreal`
# binary — resolved via $SURREAL_BIN, then ~/.surrealdb/surreal, then $PATH)
URBAN_DB_TESTS=1 cargo test --test analysis_lifecycle -p urban-integrity-cli

# Live integration tests — CH / CF API feeds (internet, no local services)
URBAN_LIVE_TESTS=1 cargo test --test live -p urban-integrity-ingestor -- --nocapture

# Coverage — HTML report
URBAN_DB_TESTS=1 cargo llvm-cov --workspace --html
xdg-open target/llvm-cov/html/index.html

# Coverage — terminal summary only
URBAN_DB_TESTS=1 cargo llvm-cov --workspace

# Coverage — single crate (faster iteration)
URBAN_DB_TESTS=1 cargo llvm-cov -p urban-integrity-core --html

# Coverage — local threshold check (exits non-zero if below 70% line coverage).
# NOT a CI gate: no CI job runs llvm-cov.
URBAN_DB_TESTS=1 cargo llvm-cov --workspace --fail-under-lines 70

# Prove the DB gate actually engaged — compare wall-clock time, not the pass count
cargo test -p urban-integrity-core --test surreal_integration                    # 0.00s: all skipped
URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test surreal_integration   # seconds: really ran

# One-shot local CI gate — runs fmt, clippy and tests even if an earlier one fails
scripts/check.sh
scripts/check.sh --db          # ... and the SurrealDB-gated suite
scripts/check.sh --fix --db    # ... after running `cargo fmt --all`

# Run with output (show println! / tracing)
cargo test --workspace -- --nocapture

# Fast type-check only (no test binary linking)
cargo check --workspace
```

`scripts/check.sh` mirrors the `test` and `lint` CI jobs (fmt, clippy, workspace tests, optionally
the gated suite). It does **not** run the `wasm-build` job's wasm32 build, Trunk build or
`scripts/check-*.sh` guards — run those by hand when you touch the frontend (see *Continuous
Integration*).

The SurrealDB launcher (`scripts/surreal-start.sh`) runs SurrealDB as an isolated, memory-capped
user systemd service (`urban-surreal`) backed by RocksDB at `rocksdb://data/surreal-rocks.db`,
bound to `[::]:8000`, detached from the calling terminal so a DB OOM cannot take the editor down
with it. It is idempotent — re-running is a no-op if a healthy instance is already up, so to
recover a wedged instance run `systemctl --user stop urban-surreal` **first**, then the launcher.

---

## Test Namespace Hygiene

Every DB-gated test runs in its own throwaway SurrealDB namespace, so tests never see each other's
rows or production data. **Take that namespace from `urban-integrity-test-support`
(`crates/test-support`) — never build one by hand.**

> The `urban-integrity-test-support` crate (`crates/test-support`) and
> `scripts/surreal-drop-test-ns.sh` are both present — re-verified on `main` at `d828be7f`,
> 2026-09-24: `TestNs::connect`, `connect_with_prefix`, `ns()`, `arc()`, `NsGuard::new`,
> `with_prefix`, `name()`, `db_tests_enabled()` and `impl Drop for NsGuard` are all still there,
> alongside the planted fixture (`planted.rs`) and seed helpers (`events.rs`). Write new DB-gated tests against this harness: the hand-rolled alternative is the leak described below.
>
> The DB those tests talk to must be started with **`scripts/surreal-start.sh`**, never
> `surreal start` directly. The launcher runs SurrealDB as a memory-capped `urban-surreal` user
> service on the RocksDB backend; a bare `surreal start` escapes both the cap and the backend, and
> has OOM-killed this project's database (and endangered the editor sharing its cgroup) more than
> once. A shell guard reroutes a bare invocation, but do not rely on it.

| Helper | Gives you | Use when |
|--------|-----------|----------|
| `TestNs::connect(db_name)` | a connected handle with `SCHEMA_DEF` already applied; derefs to `Surreal<Any>` | the test drives the DB through a `Surreal` handle |
| `TestNs::connect_with_prefix(prefix, db_name)` | the same, under a custom `test…` prefix so debris is attributable to one suite | as above, when you want to recognise the suite in `INFO FOR ROOT` |
| `NsGuard::new()` / `NsGuard::with_prefix("test…")` | the namespace **name** only | the test drives the DB through something else — a spawned CLI, a raw config struct, a second client |

`TestNs` also exposes `.arc()` (an `Arc<Surreal<Any>>` for APIs that want one — the clone does not
extend the namespace's life) and `.ns()` (the namespace name, for pointing a second client at the
same namespace). `db_tests_enabled()` from the same crate combines the `URBAN_DB_TESTS=1` check
with the TCP pre-flight.

Both helpers issue `REMOVE NAMESPACE` on **drop**, so the guard must stay bound for the whole test.
Bind it even when you only want the other half of a tuple:

```rust
// crates/ingestor/tests/planning_integration.rs
// `_ns` must stay bound for the whole test: dropping it removes the namespace.
let (db, _ns) = { /* ... connect, use_ns(guard.name()), ... */ };
```

`let (db, _) = …` discards the guard immediately and tears the namespace down underneath the
running test. Where the DB is driven through a config struct rather than a handle, bind the guard
and pass its name:

```rust
// crates/cli/tests/analysis_lifecycle.rs
let ns = unique_ns();                      // urban_integrity_test_support::NsGuard::new()
let cfg = test_config(ns.name(), None);    // `ns` lives until the end of the test
```

### Why this is load-bearing

Before the harness existed, each test file rolled its own `format!("test_{uuid}")` and nothing ever
removed it. A full `URBAN_DB_TESTS=1 cargo test --workspace` leaked a few hundred namespaces per
run, and **2,889 had accumulated in the local store by 2026-07-31.**

`NsGuard::with_prefix` asserts the prefix starts with `test`. That keeps every "is this debris?"
check a simple prefix match, makes it impossible for a guard to name a namespace the sweeper would
miss, and makes it impossible for the sweeper to touch `urban_integrity`, `urban`, or `main`.

Teardown failures are never swallowed: a failed or timed-out (>30s) `REMOVE NAMESPACE` prints
`[test-support] LEAKED namespace <name>: <error>` to stderr.

### Sweeping debris

`Drop` runs on normal return and on unwinding panics, including failed assertions. It does **not**
run when a test binary is SIGKILLed or aborts. Sweep those:

```bash
scripts/surreal-drop-test-ns.sh           # dry run (default) — lists what would go
scripts/surreal-drop-test-ns.sh --apply   # actually remove them
```

The script only ever removes namespaces whose name starts with `test`, removes them in batches of
50, and dies loudly if SurrealDB is unreachable.

### Harness overrides

| Variable | Default | Purpose |
|----------|---------|---------|
| `URBAN_TEST_DB_URL` | `http://localhost:8000` (harness, `crates/test-support/src/lib.rs`); `http://127.0.0.1:8000` (sweeper, `scripts/surreal-drop-test-ns.sh`) | endpoint the harness and the sweeper connect to — note the two defaults differ, so set the variable explicitly if your instance is not reachable on both. It does **not** move the pre-flight: `db_tests_enabled()` hard-codes `TcpStream::connect("localhost:8000")`, so pointing the variable elsewhere leaves the gate probing localhost |
| `URBAN__SURREAL__USERNAME` | `root` | root user for the test instance |
| `URBAN__SURREAL__PASSWORD` | `root` | root password for the test instance |

---

## Coverage

Coverage is measured with `cargo llvm-cov`. The headline numbers below come from a maintainers'
coverage report that is not part of the published repository — **regenerate them yourself with the
commands below rather than quoting them as current**.

```bash
# HTML report (open target/llvm-cov/html/index.html afterwards)
URBAN_DB_TESTS=1 cargo llvm-cov --workspace --html

# Terminal summary only
URBAN_DB_TESTS=1 cargo llvm-cov --workspace

# Full numbers incl. the live tier (the configuration of the recorded run below)
URBAN_DB_TESTS=1 URBAN_LIVE_TESTS=1 cargo llvm-cov --workspace --no-fail-fast --html
```

There is **no coverage gate in CI** — `.github/workflows/ci.yml` defines three jobs (`test`,
`lint` and `wasm-build`) and none of them invokes `cargo llvm-cov`. `--fail-under-lines 70` is a local convenience,
not something that will stop a merge.

The published run below is **from 2026-06-20 and badly lags the suite**: it executed 862 tests,
where the most recent measured run executed 2,573 — the report covers roughly a third of the tests
that exist, and predates the `probe`, `workbench`, `workbench-dto` and `workbench-web` crates
entirely, none of which appears in it.
Percentages are still directionally right (the shape of the per-crate gap has not changed) but
re-measure before quoting a number as current.

Latest recorded run (generated 2026-06-20 with both `URBAN_DB_TESTS=1` and
`URBAN_LIVE_TESTS=1` enabled):

| Metric | Value |
|--------|-------|
| Line coverage (workspace) | **69.3%** (19,802 / 28,574) |
| Region coverage | 67.32% |
| Function coverage | 71.18% |
| Tests executed | 862 (856 passed, 0 failed, 6 ignored) |
| Live external tests | 8 / 8 passing |

Per-crate line coverage (same run):

| Crate | Line coverage | Covered / total |
|-------|--------------:|----------------:|
| `stats` | 100.0% | 7 / 7 |
| `detector` | 98.0% | 1,245 / 1,270 |
| `schema` | 94.1% | 396 / 421 |
| `ingestor` | 76.1% | 3,486 / 4,581 |
| `enricher` | 71.7% | 6,152 / 8,578 |
| `core` | 70.6% | 4,764 / 6,747 |
| `api` | 55.5% | 262 / 472 |
| `cli` | 53.7% | 3,490 / 6,498 |

The logic crates (`stats`, `detector`, `schema`) are ≥90%. The `cli`/`api` glue crates sit lower by
design — they are exercised through the DB/E2E integration suites rather than unit-covered.

Coverage must be measured with `URBAN_DB_TESTS=1` set and SurrealDB running. Without it the 423
gated tests return at the gate, the report still shows 100% of them "passing", and the persistence
layer appears far less covered than it is. This is the skip-by-return trap wearing a different hat:
an ungated coverage run is not a weaker measurement of the same thing, it is a measurement of a
different, smaller suite that reports the same test count.

---

## Continuous Integration

`.github/workflows/ci.yml` runs on every push to `main` and on every pull request. Three jobs:

**`test` — Build + full suite (including SurrealDB integration).** Installs `protoc` (needed by
`crates/api/build.rs`, which compiles `proto/integrity.proto` via tonic-build), installs the pinned
toolchain (`rustup show` honours `rust-toolchain.toml`), starts SurrealDB `v3.1.0-beta.2` via
`surrealdb/setup-surreal@v2` (root/root on port 8000, in-memory engine, **`surrealdb_auth: true`**),
builds the workspace, then runs the **DB-gated** suite:

```bash
URBAN_DB_TESTS=1 cargo test --workspace
```

`surrealdb_auth: true` is not the action's default. Left at its default, the action passes
`--unauthenticated` and the server accepts every request as root regardless of credentials — which
silently defeats the tests that exist to check auth behaviour:
`db_query_recovers_from_a_dead_session` invalidates a session and asserts the raw connection then
fails, and the `ch_psc` / `ch_disqualified` write-propagation regressions assert a write is refused
without a signin. Under `--unauthenticated` none of those can fail for the reason claimed. Local dev
(`scripts/surreal-start.sh`) runs auth-on; CI matches it deliberately.

Running the gated tier in CI is also deliberate. These 423 DB-gated tests are skipped by a plain
`cargo test` — and because the gate is a skip-by-return, a skipped one still reports as passing — so
without this job nothing would exercise them and nothing would notice. This project has been bitten
before by gated tests marked "done" that had silently never run (several broke unnoticed).
Keeping them in CI stops them rotting.

CI's SurrealDB is the action's **in-memory** engine, started empty for each run. That is why the
half-hour cost described under *What the gated run actually costs* is invisible here and only bites
locally: `test_cli_report_markdown_creates_file` renders a report over whatever the default database
holds, which in CI is a handful of fixture rows and on an analyst machine is the full corpus. CI
being fast is therefore not evidence that the test is cheap — it is evidence that CI has no data.

This is the same shape as the local worktree incident described under *A fast gated run is the thing
to distrust* — an empty store passing everything at speed. In CI it is expected and harmless,
because CI's job is to prove the gated tests still *compile and run*, not to prove they saw the
corpus. Locally it is the bug. Do not carry a CI timing across as evidence about a local run, or a
local one as evidence about CI.

**`lint` — rustfmt + clippy.** `cargo fmt --all --check` and
`cargo clippy --workspace --all-targets -- -D warnings`. Any new clippy warning fails CI — fix it or
`#[allow]` with justification.

**`wasm-build` — frontend build and boundary guards.** Installs the pinned toolchain (including the
`wasm32-unknown-unknown` target) and Trunk `0.21.14`, then:

1. `cargo build --target wasm32-unknown-unknown -p urban-integrity-workbench-dto -p urban-integrity-workbench-web`
   — compiling for the real wasm target, not just the native `--workspace` build, is what proves
   those crates are wasm-safe.
2. `scripts/check-wasm-boundary.sh`, `scripts/check-verdict-deserialize.sh`,
   `scripts/check-no-analysis-literals.sh`, `scripts/check-fixture-not-shipped.sh` and
   `scripts/check-abbreviations.sh` — see *Frontend guards that are not `cargo test`*.
3. Node 20 plus `npm ci` in `crates/workbench-web/js-build`, then `scripts/check-shim-drift.sh`
   with `SHIM_DRIFT_STRICT=1`.
4. `trunk build --release` in `crates/workbench-web` — `dist/` is never committed, so this proves
   the frontend actually bundles.

The frontend's `cargo test` coverage (host-side unit tests and source-scanning guards) runs in the
`test` job with everything else; `wasm-build` runs no tests.

### Known CI flake and its mitigations

Under the 2-core GitHub runner, the CLI subprocess tests can contend on the single shared SurrealDB
instance. The historically flaky one is `test_cli_alerts_list_json_output_is_valid_json` (and its
sibling CLI subprocess tests): when too many run at once they hammer the DB and one can exit non-zero.
Two workflow settings keep the suite deterministic:

1. **`RUST_TEST_THREADS: "2"`** (workflow `env`) — caps per-binary test parallelism so the CLI
   subprocess tests don't all hit the DB simultaneously.
2. **Pre-initialise SurrealDB schema** — a step runs `cargo run -p urban-integrity-cli -- status`
   (with `URBAN_DB_TESTS=1`) once before the test run. `connect_db()` applies `SCHEMA_DEF` on every
   connect; without pre-seeding, the many CLI subprocesses race to run the same DDL on the fresh DB
   and can throw a transaction conflict. Pre-seeding makes those `DEFINE` statements no-ops.

The workflow also sets `RUSTFLAGS: "-C debuginfo=0"`: the enricher test binary statically links the
entire surrealdb-core + crypto + geo dep tree, and with full debuginfo the link exhausts the runner's
memory (linker SIGBUS). Dropping debuginfo roughly halves link memory.

Live external tests (`URBAN_LIVE_TESTS=1`) are intentionally **not** in CI — they hit third-party
APIs and need a Companies House REST key as a secret. Run them locally before merging ingestor HTTP
client/mapper changes.

---

## Environment Variable Guards

Three environment guards control which optional tests are enabled. None of them implies another:

| Variable | Enables | Requires |
|----------|---------|----------|
| `URBAN_DB_TESTS=1` | SurrealDB integration tests + CLI subprocess tests + probe battery tests + enricher DB tests + workbench router tests (**423** tests, census `d828be7f` 2026-09-24) | SurrealDB on `:8000` — launch via `./scripts/surreal-start.sh`, **from the repo root, never a worktree** |
| `URBAN_LIVE_TESTS=1` | 8 CH / CF API feed tests (`crates/ingestor/tests/live.rs`) | Internet access + a Companies House REST key |
| `URBAN_PLANNING_TESTS=1` | 2 planning-portal scrape tests (`crates/ingestor/tests/planning_integration.rs`) | the Scrapling sidecar on `localhost:8765` |

A fourth, `URBAN_TEST_DB_URL`, is not a gate — it moves the endpoint the harness and the sweeper
connect to. See *Harness overrides* above, including the caveat that it does not move the
pre-flight.

Most — but **not all** — `URBAN_DB_TESTS` tests also perform a **TCP pre-flight**. Tests gated on
`db_tests_enabled()` (from `urban-integrity-test-support`) check both the variable *and* that
SurrealDB is listening on `localhost:8000`, so an unreachable DB makes them return cleanly rather
than panic.

A sizeable minority perform **no** TCP check and then call `TestNs::connect(...)`, which
`.expect("SurrealDB unreachable …")`. With `URBAN_DB_TESTS=1` set and the DB down, those **panic**
rather than skip. Two shapes get you there: a bare `std::env::var("URBAN_DB_TESTS")` read inside
the test, and — the newer trap — a **file-local `db_tests_enabled()` that reads the variable and
stops there**, sharing its name with the pre-flighting one in `urban-integrity-test-support`. The
call site looks identical either way; only the definition differs. Census on `main` at `722b568`,
2026-08-25:

| File | Tests gated without a pre-flight | Gate shape |
|------|---------------------------------:|------------|
| `crates/enricher/tests/engine_coverage.rs` | 12 | local `db_tests_enabled()`, no TCP check |
| `crates/enricher/tests/psc_adapter.rs` | 8 | local `db_tests_enabled()`, no TCP check |
| `crates/enricher/tests/disqualified_adapter.rs` | 6 | local `db_tests_enabled()`, no TCP check |
| `crates/enricher/src/adapters/ch_disqualified.rs` | 5 | bare `std::env::var` |
| `crates/enricher/src/adapters/ch_psc.rs` | 5 | bare `std::env::var` |
| `crates/enricher/tests/property_accumulation.rs` | 5 | bare `std::env::var` |
| `crates/enricher/src/land_flip.rs` | 4 | bare `std::env::var` |
| `crates/api/tests/server.rs` | 2 | bare `std::env::var` |
| `crates/enricher/tests/ccod_adapter.rs` | 1 | bare `std::env::var` |
| `crates/cli/src/commands/migrate.rs` | 1 | bare `std::env::var` |
| `crates/enricher/src/adapters/land_registry_ppd.rs` | 1 | bare `std::env::var` |
| `crates/enricher/tests/gazette_adapter.rs` | 1 | bare `std::env::var` |
| **Total** | **51 of 215** | |

**That census has not been re-run.** The denominator has since moved from 215 to 423 (`d828be7f`,
2026-09-24), so read the table as "these 51 gate sites had no pre-flight on
2026-08-25", not as a current proportion. The files listed are all `enricher`, `api` and `migrate`;
none of the probe, workbench or profile tests added since appears in it, but that is an absence of
evidence until the census is re-run.

48 of them are in `enricher`; the other three are `crates/api/tests/server.rs` (2) and
`crates/cli/src/commands/migrate.rs` (1). None of the Tier-1 probe tests was in this list — every
one of them pre-flights. The other 164 are pre-flighted: they call
`urban_integrity_test_support::db_tests_enabled()`, or one of the many file-local equivalents that
do the same `TcpStream::connect("localhost:8000")` check — `skip_if_no_db()` in
`crates/cli/tests/cli.rs`, and a per-file `db_tests_enabled()` in most of the integration test
files. `crates/ingestor/src/backfill/mod.rs` is the one that probes `[::1]:8000` and
`127.0.0.1:8000` rather than `localhost:8000`.

`crates/cli/src/commands/migrate.rs` contains **both** kinds, which is why earlier revisions of this
document contradicted themselves about it:
`reauth_db_recovers_from_a_genuinely_expired_token` (in `mod tests`) calls
`urban_integrity_test_support::db_tests_enabled()` and is pre-flighted, while
`located_at_accepts_contract_via_schema_def_and_migration_ddl` (in `mod tests`) reads
`std::env::var("URBAN_DB_TESTS")` directly and is not. Attribute the gate per test function, not per
file.

So a plain `cargo test --workspace` (variable unset) always exits 0 regardless of whether the DB
is running — but `URBAN_DB_TESTS=1 cargo test --workspace` without a reachable SurrealDB does not.
Start the DB before setting the flag.

That convenience is also the trap: because the gate is a skip-**by-return**, a skipped test counts
as a passing test. See *The DB gate is a skip-by-return* above — verify the gated tier by comparing
wall-clock runtime, never by comparing pass counts.

---

## The Planted Fixture and the Toy Analysis

Most DB-gated tests seed a handful of rows into a throwaway namespace and assert on those. The
workbench, the probe battery's evidence goldens and the end-to-end chain need something larger: a
graph whose correct answers are **known by construction**. That is the planted fixture.

**Where it lives.** `crates/test-support/src/planted.rs`:

- `planted_graph()` returns a `PlantedGraph` — the nodes and edges (every id prefixed `planted`,
  e.g. `director:planted-o1`, `location:planted-l2`, `contract:planted-c1`; company numbers in the
  reserved `GB-COH:PL000…` range) together with the identity links, motif events and land-flip run
  records the probes need.
- `Manifest` holds the expected answers as **literals**: shortest paths and no-path cases, all-
  simple-path counts under a hop cap, Louvain community sets, k-core membership, the exact top
  betweenness score, the ward subgraph, side-card and profile samples, disambiguation and
  neighbourhood expectations, and per-probe evidence reference counts. The seeded size is part of
  it (`seeded_nodes: 89`, `seeded_edges: 102` at `d828be7f`). Nothing in the manifest is computed
  from the graph at test time — a manifest derived from the implementation under test would agree
  with any bug in it.
- The fixture is anchored on a synthetic ward, `WARD_CODE = "X05999001"`, and a synthetic LPA,
  `LPA_CODE = "X06999001"`, so it can never collide with a real ONS code.

`crates/test-support/src/events.rs` holds the seed helpers (`seed_node_event`, `seed_edge_event`,
`seed_identity_link`, `seed_motif_event`, `seed_land_flip_run`, …) and `seed_planted`, which writes
the whole fixture into a namespace in one call. All seeded events carry the fixed
`FIXTURE_TIMESTAMP_MS`.

**Who consumes it:**

| Consumer | What it proves | DB |
|----------|----------------|----|
| `crates/workbench/tests/planted_golden.rs` (11 tests) | every graph algorithm reproduces the manifest through `dispatch::run_algorithm` (the real HTTP entry point's core), plus one router round trip | no |
| `crates/workbench/tests/planted_profile.rs` (5) | the entity-profile screen's sections against the manifest | yes |
| `crates/workbench/tests/finding_star_golden.rs` (1) | the real report pipeline carries the address-cluster star motif's centre and eight leaves through to the view | yes |
| `crates/workbench/tests/planted_examples_match_manifest.rs` (2) | every worked example the help pages render (literals in `crates/workbench-dto/src/planted_examples.rs`) is a fact about the fixture | no |
| `crates/workbench/tests/population_equals_export.rs` (1) | the workbench's ward-reachability walk and `export-graphml --reachable-from-wards` produce the same entity set | yes |
| `crates/workbench/tests/degree_distribution.rs` (2) | prints degree percentiles for the population; deliberately asserts no fixed number | partly |
| `crates/probe/tests/planted_evidence_refs.rs` (10) | the evidence reference count per finding on the planted fixture | yes |
| `crates/cli/tests/toy_analysis_chain.rs` (1) | the whole chain — provision, seed, probe, boot the workbench, assert population and algorithm results over HTTP | yes |

`crates/workbench/src/graph/fixture.rs` exposes `workbench_from_graph`, a `#[doc(hidden)]`
assembler that builds a coordinator handle and `WorkbenchState` straight from a node/edge list so
integration tests can drive `boot::build_router` without SurrealDB (and a variant that takes a real
`TestNs` handle for the routes that read persistence). It fabricates a population, so it must never
be reachable from shipped code; `scripts/check-fixture-not-shipped.sh` fails CI if the symbol is
mentioned anywhere outside its definition, the test trees and prose.

### The toy analysis

The same fixture can be seeded into a real, named analysis database for manual checks of the
workbench:

```bash
# Requires SurrealDB on :8000 (scripts/surreal-start.sh, from the repo root).
scripts/toy-analysis.sh
# then, with the slug it prints:
cargo run -p urban-integrity-cli -- workbench --analysis <slug>
```

The script creates an analysis labelled "Planted fixture" with `analysis create --fixture` (keyed
on the fixture's ward code), runs `fixture seed --planted --reseed`, `project`, and `probe
--output data/toy-evidence-report.md`. It is idempotent: re-running re-seeds and re-projects the
same analysis. `fixture seed` **refuses** any analysis that was not created with `--fixture`, so the
fixture cannot be written into a real corpus by mistake.

Re-seed before relying on the toy analysis — a stale toy database from an older fixture reads as
"the fixture is missing things" when it is only out of date. The script runs several `cargo run`s,
so it counts as a cargo invocation for the one-at-a-time rule above.

---

## Analyst Workbench Tests

The browser workbench is three crates, and each is tested differently.

### `crates/workbench-dto` — 87 inline tests, no DB

The serde-only wire types shared by the server and the wasm frontend. Tests are round-trips and
shape pins: a DTO serialises to the JSON the other side expects, optional fields default, and
enumerations stay closed. One rule matters beyond this crate: **a field added to a DTO that is
persisted must carry `#[serde(default)]`** — a required field makes every previously stored row
undeserialisable. Requests may stay strict (a malformed request is rejected); stored rows must
default. Pin the change with a test that deserialises a pre-change fixture.

### `crates/workbench` — 235 tests, 30 DB-gated

The Axum server. Inline tests cover the graph algorithms (`graph/algorithms/`: paths,
community, k-core, betweenness, ward subgraph), the dispatch layer (`graph/dispatch.rs`, 27), the
canvas filter, label index, session cache and cancellation, asset serving (`assets.rs`, 12) and boot
(`boot.rs`, 12, 2 DB-gated).

The integration tests under `crates/workbench/tests/` are **router-level by rule**: they spawn
`boot::build_router` on an ephemeral port and talk HTTP to it, rather than calling a handler
function. A test that calls a handler proves the function works and says nothing about whether the
route is wired or spelled the way the browser calls it.

| File | Tests | DB-gated | Covers |
|------|------:|---------:|--------|
| `entity_api.rs` | 12 | 8 | entity profile, neighbourhood and disambiguation routes |
| `planted_golden.rs` | 11 | 0 | algorithms vs the planted manifest (see above) |
| `planted_profile.rs` | 5 | 5 | profile sections vs the manifest |
| `canvas_filter_api.rs` | 3 | 2 | a saved canvas contains exactly what was visible |
| `report_persistence.rs` | 3 | 3 | the cached report round-trips byte-identical |
| `investigation_api.rs` | 2 | 2 | a saved investigation survives a restart; pinning twice does not duplicate |
| `report_view_equals_finding_list.rs` | 2 | 2 | the report view and the Markdown writer receive the same findings |
| `planted_examples_match_manifest.rs` | 2 | 0 | help-page worked examples vs the manifest |
| `degree_distribution.rs` | 2 | 1 | prints degree percentiles (no fixed assertion) |
| `finding_star_golden.rs` | 1 | 1 | star-motif evidence through the real report pipeline |
| `population_equals_export.rs` | 1 | 1 | two independent reachability walks agree |
| `regenerate_population_arrow.rs` | 1 | 0 | `#[ignore]` — regenerates the frontend's Arrow fixture |

The workbench embeds the Trunk `dist/` output with `rust-embed` (`debug-embed`, so even debug
builds embed at compile time). A binary built before the last `scripts/build-workbench.sh` serves
the old frontend; rebuild the Rust binary after rebuilding `dist/` before any visual check.

`crates/cli/tests/workbench_boot.rs` (3 tests, DB-gated) spawns the compiled CLI's `workbench`
subcommand as a real subprocess and asserts at process level that it never starts `serve`'s
ingestors and that a taken `--bind` address exits non-zero naming the address.

### `crates/workbench-web` — 550 tests, none DB-gated, no browser

The Leptos 0.8 client-side-rendered frontend. It compiles to `wasm32-unknown-unknown` for the
browser, but **every test runs host-side** as an ordinary `#[test]` under `cargo test`. There is no
headless browser, no DOM runner and no `wasm-bindgen-test` in this workspace. That boundary decides
what the tests can prove:

- **Inline unit tests (`src/**`, 480)** call pure functions for real — formatting, filter
  predicates, status-line sentences, href builders, palette choices, route parsing. The largest
  blocks are `graph_page.rs` (35), `graph_status_line.rs` (30), `tool_rail.rs` (29),
  `profile/sections.rs` (28) and `analysis_page.rs` / `graph_results_list.rs` (25 each).
  `graph_page.rs` also parses `src/testdata/population-real-arrow.bin`, a real Arrow IPC payload
  produced by the server's encoder (see the ignored regeneration test above).
- **Source-scanning guard tests (`tests/`, 70 across 10 binaries)** read the crate's own source
  with `include_str!` or a directory walk and assert on its text. `workbench-web` is a binary crate
  with no library target, so an integration test cannot call its functions; it can only read them.
  A source sweep can prove **wiring** — that a component is mounted, that a value comes from one
  place, that a second definition does not exist. It cannot prove **liveness** — that anything
  looks right in a browser. That is what the manual review on the toy analysis is for.

Why the sweeps exist: a fully built, unit-tested, CI-guarded abbreviation component once shipped
with **zero call sites**, and every test stayed green, because every test asked whether the
component worked rather than whether anything used it. An "every X in the UI" rule needs a test
that finds call sites.

| File | Tests | Asserts |
|------|------:|---------|
| `abbr_reachable.rs` | 17 | the `Abbr` component is called where abbreviations render, and no shipped `view!` literal carries a bare glossary abbreviation; the sweep's own file registry is checked against a walk of `src/` |
| `pivot_reachable.rs` | 9 | every surface that renders an entity reference renders it as a link through the one shared href builder, and the linked record classes are exactly the ones the read path can open |
| `expansion_reachable.rs` | 8 | there is exactly one profile renderer, mounted at two densities; the expansion cap reaches the status line; the hop control is mounted |
| `investigation_reachable.rs` | 8 | the pin control is mounted in the profile renderer, the investigation list is reachable from every route, delete takes a named confirm step |
| `alert_controls_affordance.rs` | 7 | WCAG non-text contrast (≥ 3:1) of control borders, computed from `style/tokens.css`, and every `/alerts` control button carries the control class |
| `cvd_node_palette.rs` | 5 | the node palette's closest pair stays distinguishable under simulated protanopia, deuteranopia and tritanopia in both themes |
| `routes_render.rs` | 4 | each expected `(path, component)` pair appears exactly once in the route table, and the total route count is exact |
| `alert_filters_rail_stable.rs` | 4 | the `/alerts` filter rail is mounted once, outside any closure that would rebuild it (and drop input focus) on every facets update |
| `alert_detail_preview.rs` | 4 | the alert detail's preview box draws the motif rather than a chip list |
| `canvas_filter_reachable.rs` | 4 | there is one definition of what is visible on the canvas, and saving reads it |

**Writing a source-scanning test here.** Follow the patterns already in these files:

- **Strip comments before matching.** A comment that mentions a symbol must not satisfy a check
  that the symbol is used. Most files carry a `without_comments` / `strip_line_comments` helper.
- **Scan only the shipped portion** of each file — the text before its `#[cfg(test)]` module — so a
  test fixture cannot satisfy a production-code assertion (`shipped_portion`).
- **Check the registry against the tree.** A sweep over a hand-listed set of files silently skips
  the next file someone adds; walk `src/` and fail if a relevant file is not registered.
- **Match bare names, not calls.** A sweep looking for `foo(` is walked past by a bare `foo`
  mention; search for the token the requirement actually forbids.
- **Prove non-vacuity by mutation.** Each file's header records the one-line mutation (delete a
  route, replace an `<Abbr>` with a bare literal) that makes it fail. Do that once by hand and
  revert. A detector that has never been seen to fail may pass by being broken.
- The helpers are **copied between files rather than shared** — `workbench-web` has no library
  target to host them.

```bash
cargo test -p urban-integrity-workbench-web                          # all 550, seconds, no services
cargo test -p urban-integrity-workbench-web --test pivot_reachable   # one sweep
```

### Frontend guards that are not `cargo test`

The CI `wasm-build` job adds checks that only exist as scripts. Run them locally when you change the
frontend or its dependencies:

| Script | Fails when |
|--------|------------|
| `scripts/check-wasm-boundary.sh urban-integrity-workbench-dto urban-integrity-workbench-web` | `surrealdb`, `tokio`, `petgraph`, `rayon`, or the `schema`/`core`/`detector` crates enter the wasm32 dependency graph of a workbench-facing crate |
| `scripts/check-verdict-deserialize.sh` | `Deserialize` reappears on the schema crate's verdict type, or on a bare-named verdict-domain type in a workbench crate |
| `scripts/check-no-analysis-literals.sh` | an analysis-specific string leaks into workbench source |
| `scripts/check-fixture-not-shipped.sh` | `workbench_from_graph` is referenced outside its definition, the test trees and prose |
| `scripts/check-abbreviations.sh` | a `workbench-web` string literal contains an unexpanded 2-6 letter all-caps abbreviation not defined in `docs/abbreviations.md` or listed in `scripts/abbreviations-allowlist.txt` |
| `scripts/check-shim-drift.sh` | the vendored Cosmograph shim bundle (`crates/workbench-web/js/cosmograph-shim.js`) differs from what its source in `js-build/` builds to; needs Node and `npm ci` in `crates/workbench-web/js-build`, and `SHIM_DRIFT_STRICT=1` makes a missing toolchain a failure instead of a skip |

`check-abbreviations.sh` proves a token is *documented*; `abbr_reachable.rs` proves it is
*rendered through the component*. Neither implies the other, which is why both exist.

---


## Test Inventory

### `crates/core` Unit Tests

```
graph::snapshot::tests::test_bake_csr_empty_storage          ✅
graph::snapshot::tests::test_bake_csr_baked_at_event_recorded ✅
graph::snapshot::tests::test_bake_csr_edge_count_matches_storage ✅
graph::snapshot::tests::test_bake_csr_node_count_matches_storage ✅
graph::snapshot::tests::test_snapshot_handle_empty_initialises ✅
graph::snapshot::tests::test_snapshot_handle_store_rotates_snapshot ✅
graph::snapshot::tests::test_snapshot_handle_concurrent_read  ✅
graph::storage::tests::test_insert_node_dedup                ✅
graph::storage::tests::test_insert_edge_requires_existing_nodes ✅
persistence::checkpoint::tests::test_checkpoint_roundtrip    ✅
persistence::dead_letter::tests::test_log_dead_letter_*      ✅ (4 tests)
persistence::wal::tests::test_drain_batch_*                  ✅ (4 tests — disconnected_returns_empty, idle_channel_timeout, single_event, respects_max_batch)
pipeline::tests::test_pipeline_*                             ✅ (5 tests — happy_path, rejects_empty_id, rejects_empty_name, rejects_self_loop, normalises_name)
config::tests::test_load_default_config                      ✅
```

### `crates/detector` Unit Tests

```
motif::phoenix::tests::test_phoenix_*                        ✅ (3 tests — detector_creation, check_tier_a, check_tier_b)
motif::phoenix::tests::corporate_officers_do_not_carry_a_phoenix_motif ✅ corporate-officer roles excluded from the motif
motif::phoenix::tests::test_phoenix_window_within_fires      ✅ 90-day gap → detection fires
motif::phoenix::tests::test_phoenix_window_outside_silent    ✅ 400-day gap → no detection
motif::phoenix::tests::test_phoenix_window_outside_does_not_fire ✅ live 190-day award/dissolution gap → no detection
motif::nepotism::tests::test_nepotism_*                      ✅ (6 tests — detector_creation, check_empty, detects_direct_link, detects_two_hop_link, respects_max_hops, no_false_positive_without_political_actor)
motif::cluster::tests::test_cluster_*                        ✅ (16 tests — detector_creation, check_empty, detects_three_entities_at_one_address, detects_three_planning_applicants_at_one_address, below_threshold_no_alert, score_scales_with_entity_count, excludes_pure_buyers_at_shared_gov_address, still_fires_for_suppliers_at_one_address, counts_only_suppliers_when_buyers_share_address, suppressed_at_formation_agent_postcode, suppressed_at_ch_default_address, normal_address_not_suppressed, detects_contract_sourced_address_cluster_attributed_to_applicant, dedups_mixed_legacy_and_contract_sourced_located_at, excludes_placeholder_applicant_via_contract_sourced_path, ignores_non_entity_neighbors)
motif::cluster::tests::test_placeholder_applicant_*          ✅ (2 tests — a placeholder applicant is not counted as a bidder, and does not suppress a genuine cluster)
motif::cluster::tests::test_postcode_of_loc_id               ✅  postcode extracted from a location EntityId
motif::cluster::tests::test_ch_default_address_marker        ✅  the CH default-address marker is recognised
motif::cluster::tests::test_embedded_postcode_list_parses    ✅  the embedded formation-agent postcode list parses
motif::cluster::tests::test_parse_postcode_list_normalises   ✅  postcode-list entries normalise before comparison
                                                             (22 tests total in crates/detector/src/motif/cluster.rs)
alert::buffer::tests::test_buffer_*                          ✅ (2 tests — insert_pop, ttl_cleanup)
alert::cache::tests::test_alert_cache_eviction               ✅
alert::result::tests::test_motif_hash_is_stable_and_order_independent ✅
scoring::formula::tests::test_compute_*                      ✅ (3 tests — p_base, deprivation, p_final)
scoring::formula::tests::imd_weighting_is_a_tiebreak_not_a_saturator ✅ headroom form cannot reach 1.0 from below
scoring::formula::tests::both_multiplicative_bands_would_saturate ✅ the replaced multiplicative form saturates — the regression this guards
scoring::formula::tests::effective_m_env_reports_what_actually_happened ✅ m_env reflects the uplift applied, not the one configured
scoring::registry::tests::test_score_registry_*              ✅ (2 tests — threshold, empty)
scoring::imd::tests::test_imd_load_and_deprivation           ✅  index loads, ward rank → deprivation fraction
scoring::imd::tests::a_declared_denominator_beats_the_max_row ✅  the declared `IMD Denominator` wins over the observed maximum
scoring::imd::tests::an_undeclared_denominator_falls_back_and_is_flagged ✅ missing column → fallback, flagged not silent
scoring::imd::tests::an_inconsistent_denominator_is_rejected ✅  rows disagreeing about the denominator → load refused
scoring::imd::tests::a_rank_beyond_the_denominator_is_rejected ✅  out-of-range rank → rejected
scoring::imd::tests::an_unknown_ward_is_distinguishable_from_the_least_deprived ✅ no-match ≠ rank-last
scoring::imd::tests::extra_columns_are_ignored               ✅  unexpected CSV columns do not break the load
scoring::imd::tests::index_reports_the_nation_of_its_wards   ✅  the index knows which nation it belongs to
scoring::imd::tests::an_empty_index_has_no_nation            ✅  empty index → None, not a guess
scoring::imd::tests::an_index_spanning_nations_reports_no_nation ✅ mixed-nation index → None, which the preflight refuses
```

> `alert::cache` has 5 further tests beyond `test_alert_cache_eviction` — see
> *`crates/detector` — Additional Inline Unit Tests* below.

### Integration Tests

```
crates/detector/tests/integration.rs::test_phoenix_loop_end_to_end ✅
crates/detector/tests/integration.rs::test_imd_end_to_end         ✅
```

### API Server Tests (`crates/api/tests/server.rs`)

In-process Axum server on an ephemeral port — real HTTP requests, no mocks. 16 tests
(15 active + 1 `#[ignore]`, 2 of them DB-gated); the GQL-numbered tests cover the v2.0 GraphQL
entity surface:

```
test_health_returns_200_and_ok_status                         ✅  /health → status=ok, version present
test_health_empty_graph_returns_zero_counts                   ✅  /health → node_count=0, edge_count=0
test_metrics_returns_200_and_graph_stats                      ✅  /metrics → all fields present
test_graphql_graph_stats_query                                ✅  graphStats { nodeCount edgeCount }
test_graphql_alerts_query_returns_empty_list                  ✅  alerts → [] (stub)
test_graphql_alert_by_id_returns_null_not_error               ✅  alert(nonexistent) → null
test_graphql_rejects_over_depth_query                         ✅  depth limit handled gracefully
test_graphql_entity_query                                     ✅  entity(name: unknown) → null, not an error (no DB in test context)
test_graphql_entity_by_ch                                     ✅  entity(chNumber:) → null without errors when DB absent
test_graphql_entity_schema_fields                             ✅  EntityProfile introspection shows all required fields
test_graphql_graph_path                                       ✅  graphPath(from:, to:) → empty list, not an error, when DB absent
test_graphql_entity_profile_type_registered                   ✅  EntityProfile registered in schema (EntityLoader wiring prerequisite)
test_graphql_introspection_includes_entity_types              ✅  schema introspection returns all v2.0 type names
test_graphql_with_live_data_from_contracts_finder             ⏭  #[ignore = "Requires live internet access to contractsfinder.service.gov.uk"] — run with --ignored
the_api_does_not_publish_a_withdrawn_alert                     ✅  (DB) a withdrawn alert is absent from the GraphQL alert surface
grpc_get_alert_does_not_publish_a_withdrawn_alert              ✅  (DB) the same guarantee on the gRPC GetAlert path
```

Both withdrawn-alert tests gate on a bare `std::env::var("URBAN_DB_TESTS")` with no TCP pre-flight,
so they panic rather than skip if the flag is set and SurrealDB is down.

### CLI Tests (`crates/cli/tests/cli.rs`)

The file has grown to 44 tests across three groups:

**Output formatter unit tests** (pure function):
```
test_severity_color_*                                         ✅ (3 tests — red/yellow/green thresholds)
test_build_alert_table_*                                      ✅ (5 tests — empty, single row, none ward, multiple rows, entities column)
test_print_json_*                                             ✅ (2 tests — struct, empty vec)
```

**Binary smoke tests** (13, subprocess, no DB):
```
test_cli_help_exits_zero                                      ✅
test_cli_report_help_exits_zero                               ✅
test_cli_alerts_help_exits_zero                               ✅
test_cli_status_help_exits_zero                               ✅
test_cli_entity_help_exits_zero                               ✅
test_cli_entity_no_args_exits_nonzero                         ✅
test_cli_entity_help_contains_jq_example                      ✅  --help output includes a jq pipe example
test_cli_entity_plain_name_accepted_as_arg                    ✅  plain company name accepted as positional arg
test_cli_ingest_planning_help_exits_zero                      ✅
test_cli_ingest_planning_no_args_exits_nonzero                ✅
test_cli_invalid_subcommand_exits_nonzero                     ✅
test_cli_report_output_file_with_json_format_exits_nonzero    ✅  --output-file rejected with JSON format
test_cli_report_bad_url_exits_nonzero_with_message            ✅  unreachable DB → non-zero exit with "SurrealDB" in stderr
```

**CLI DB subprocess tests** (21, require `URBAN_DB_TESTS=1` + running SurrealDB):
```
test_cli_report_table_output_contains_header                  ✅  table format has "Motif" + "Total alerts:"
test_cli_report_with_ward_filter_accepts_arg                  ✅  --ward E05001234 reflected in JSON
test_cli_report_zero_min_score_returns_all                    ✅  --min-score 0.0 returns all events
test_cli_report_json_output_is_valid_json                     ✅  JSON with generated_at, total_alerts, alerts[]
test_cli_report_markdown_creates_file                         ✅  --format md writes the report file
test_cli_alerts_list_json_output_is_valid_json                ✅  JSON array output
test_cli_alerts_list_limit_flag                               ✅  --limit 5 returns ≤5 results
test_cli_alerts_list_since_flag_json_output                   ✅  --since filter accepted, JSON output valid
test_cli_alerts_list_ward_filter_empty_result                 ✅  unmatched ward filter → empty list, exit 0
test_cli_alerts_list_motif_filter                             ✅  --motif filter applied
test_cli_alerts_show_unknown_id_exits_nonzero                 ✅  unknown alert ID → non-zero exit
test_cli_status_shows_db_connected                            ✅  JSON with node_count, edge_count
test_cli_status_table_output                                  ✅  table format renders
test_cli_entity_json_output_is_valid_json                     ✅  entity subcommand emits valid JSON
test_cli_entity_json_fields_present                           ✅  expected profile fields present
test_cli_entity_pipe_defaults_to_json                         ✅  non-TTY stdout defaults to JSON
test_cli_entity_json_dates_are_iso8601                        ✅  dates serialised ISO-8601
test_cli_entity_slug_dispatch_round_trip                      ✅  slug-form arg dispatches to id lookup
test_cli_entity_slug_fallback_to_name                         ✅  unmatched slug falls back to name search
test_cli_ingest_planning_unknown_ward_exits_nonzero           ✅  --ward with an unconfigured code → non-zero exit (opens with `skip_if_no_db()`, cli.rs:788)
purge_refuses_non_tty                                         ✅  `purge-orphans` spawned with stdin closed exits non-zero and leaves the seeded orphan row in place
```

`purge_refuses_non_tty` is the confirmation-prompt guard: the purge path can only be confirmed
from a real terminal, and there is deliberately no `--yes`, no `--force` and no environment
escape. See *OCOD Owner Merge and the Purge Path* below for the rest of that suite.

### OSINT Surface Tests (`crates/cli/tests/osint_surface.rs`)

OSINT enrichment surface tests. No DB required:

```
osint_section_skeleton_empty_profile_renders_all_headings     ✅  all 6 subsection headings present in block
osint_section_skeleton_empty_profile_all_none                 ✅  6 "(none)" entries for empty profile
osint_section_skeleton_populated_beneficial_owner             ✅  "John Smith" + natures_of_control visible; 5 subsections still "(none)"
verify_osint_flags_disqualified_director_flag                 ✅  has_disqualified_director=true when disqualifications non-empty
verify_osint_flags_active_evasion_overrides_critical          ✅  confirmed active evasion → risk_level=Critical
verify_osint_flags_critical_maps_to_exit_1                    ✅  Critical/High risk → matches! predicate fires exit(1)
verify_osint_flags_phase17_stubs_are_false                    ✅  has_offshore_link=false, has_sanctions_match=false (stubs when these flags were introduced)
verify_osint_flags_beneficial_owners_populated                ✅  beneficial_owners populated from EntityProfile
verify_osint_flags_no_evasion_no_override                     ✅  no active evasion → risk_level stays Low
```

### Hybrid Validation (`crates/core/tests/hybrid_validation.rs`)

```
test_logic_pressure_hybrid   ✅  (completes in ~0.11s)
```

This test is the primary correctness gate:
1. Spins up a full `GraphCoordinator` with a real `DetectorEngine`
2. Injects a "Golden Record" Phoenix pattern: `Active Entity → Officer → Dissolved Entity`
3. Pumps 500 concurrent synthetic noise events
4. Asserts a `MotifDetected` WAL event fires within 10 seconds

### UpdateNode / CSR Propagation (`crates/core/tests/coordinator_update_node.rs`)

Pure in-memory tests — no SurrealDB required. Verifies the UpdateNode round-trip and CSR snapshot propagation:

```
coordinator_update_node_changes_weight     ✅  AddNode then UpdateNode → weight reflects InLiquidation status
coordinator_update_node_emits_wal_event    ✅  UpdateNode emits a WAL event of the correct kind
coordinator_update_node_no_topology_change ✅  UpdateNode changes weight but not node count or edge topology
coordinator_update_node_noop_on_missing    ✅  UpdateNode on unknown EntityId → no panic, no WAL event
snapshot_entity_status                     ✅  CSR snapshot reflects updated EntityStatus after bake
```

### Performance CI Gate (`crates/core/tests/performance.rs`)

```
test_perf03_p99_under_10ms   ✅  P99≈2.5µs — ~4,000× under the 10ms SLA (a fresh run measured 2.043µs)
```

Hard CI gate: spawns 10,000 concurrent Tokio tasks, each loading the CSR snapshot via `SnapshotHandle.load()` (ArcSwap). Measures P99 latency. Fails if P99 exceeds 10ms. Run with `cargo test -p urban-integrity-core -- test_perf03_p99_under_10ms`.

---

## SurrealDB Integration Tests

These tests exercise the complete persistence layer against a live SurrealDB instance reachable over the Rust SDK pinned at `=3.1.0-beta.2` (see workspace `Cargo.toml`). They require `URBAN_DB_TESTS=1` and SurrealDB listening on `localhost:8000` (root/root credentials). Start the DB with `./scripts/surreal-start.sh`.

Each test takes an isolated namespace from `urban-integrity-test-support`: `TestNs::connect(...)` connects, selects a fresh `test_{uuid}` namespace and database, applies `SCHEMA_DEF`, and removes the namespace when the handle drops — so concurrent runs and reruns cannot interfere, and nothing is left behind. See [Test Namespace Hygiene](#test-namespace-hygiene). All tests perform a TCP pre-flight, so an unreachable DB skips them cleanly rather than panicking.

> **SurrealDB 3.x workaround:** The Rust SDK (`=3.1.0-beta.2`) has a RecordId coercion issue — string values matching the `table:key` pattern are auto-coerced to `RecordId` on write, causing equality comparisons like `payload.entity_id = $id` to fail at query time (stored type is `RecordId`, bound type is `String`). Integration tests that involve entity-id lookups use per-contract targeted queries with Rust-side filtering (fetch all matching rows by `kind`/`weight_json` CONTAINS, then filter in Rust) rather than `payload.entity_id = $id` WHERE clauses. Use `http://localhost:8000` (not `ws://`) in test helpers — the SDK connects more reliably over HTTP.

### `crates/core/tests/surreal_integration.rs` (35 tests, 34 DB-gated at `d828be7f`)

Exercises alert queries, entity/officer queries, related-entity discovery, beneficial-owner and OSINT-link resolution, graph-traversal paths, LPA presence bucketing, and security input validation against a schema-initialised SurrealDB:

**Alert queries (core alert filter suite — 14 tests):**
```
test_schema_creates_graph_events_table                        ✅  graph_events table exists after schema init
test_schema_creates_ch_cursor_table                           ✅  ch_cursor table exists after schema init
test_query_alerts_empty_db_returns_empty_not_error            ✅  no "table does not exist" error on fresh DB
test_get_alert_by_id_missing_returns_none                     ✅  unknown ID → None, not Err
test_get_alerts_by_ids_empty_input_returns_empty_map          ✅  empty IDs slice → empty map
test_query_alerts_returns_seeded_motif_events                 ✅  seeded event returned by query_alerts
test_query_alerts_min_score_filter                            ✅  score 0.8 returned, 0.3 excluded (threshold 0.7)
test_query_alerts_score_boundary_exact_match                  ✅  score == threshold included
test_query_alerts_ward_filter                                 ✅  wrong ward code excluded
test_query_alerts_motif_type_filter                           ✅  wrong motif_type excluded
test_query_alerts_limit_respected                             ✅  limit 3 from 10 seeded events
test_query_alerts_combined_ward_and_score_filter              ✅  both filters applied
test_get_alert_by_id_found                                    ✅  correct record returned
test_get_alerts_by_ids_batch_fetch                            ✅  batch of 2 IDs returned, partial miss handled
```

**Entity queries:**
```
test_query_entity_by_name_returns_profile                     ✅  case-insensitive name search → EntityProfile
test_query_entity_by_name_multiple_matches                    ✅  "Acme" → ≥2 matches, capped at 10
test_query_entity_by_ch_number_exact_match                    ✅  exact CH number → Some(profile)
test_query_entity_by_ch_number_not_found                      ✅  unknown CH number → None
test_query_entity_by_ch_number_regression_planner_bug         ✅  regression: SELECT * returns the row even when the idx_graph_events_kind + CONTAINS planner path previously returned 0 rows while count()=2
test_query_entity_beneficial_owners_from_controls             ✅  Controls edges → beneficial_owners populated on EntityProfile
test_slug_branch_falls_back_to_name_search                    ✅  non-existent slug → None from both id and name search
test_query_entity_by_name_resolves_director_slug_across_lpas  ✅  global director slug accumulates contracts from 2 LPA wards
test_query_related_entities_via_shared_director               ✅  shared officer → related entity with kind="director"
test_query_related_entities_via_shared_address                ✅  shared location → related entity with kind="address"
test_query_related_entities_empty_when_no_connections         ✅  isolated entity → empty related list
test_query_entity_by_name_handles_injection_attempt           ✅  SQL injection string → Ok(empty), DB intact
test_query_entity_by_ch_number_rejects_injection              ✅  is_valid_ch_number gate → Ok(None), DB intact
```

**Property & OSINT-link resolution:**
```
test_query_entity_owns_property                               ✅  OwnsProperty edges → property records resolved onto EntityProfile
test_query_entity_ec_osint_links                              ✅  Electoral Commission identity_links surfaced on the entity OSINT profile
```

**Graph traversal:**
```
test_graph_trustee_of_in_position                             ✅  TrusteeOf relation materialises with the officer in the IN position
test_graph_path_offshore_triangle                             ✅  shortest-path traversal across an offshore-link triangle resolves
```

**Alert withdrawal:**
```
a_withdrawn_alert_is_not_served_on_the_entity_path            ✅  a withdrawn alert does not reappear on the entity read path
```

**LPA presence:**
```
lpa_presence_buckets_by_lpa                                   ✅  contracts bucketed by LPA via config ward→LPA map; unmapped → "Unknown LPA"
ward_to_lpa_skips_none_lpa_code                               ✅  (unit, no DB) portals with lpa_code: None excluded from ward→LPA map
```

### `crates/ingestor/tests/cursor_integration.rs` (8 tests)

Exercises `SurrealCursorStore` — the Companies House stream timepoint cursor backed by SurrealDB:

```
test_cursor_load_on_empty_db_returns_none                     ✅  fresh namespace → None, not Err
test_cursor_persist_and_load_round_trip                       ✅  persist(42) → load() = Some(42)
test_cursor_persist_zero_timepoint                            ✅  zero is a valid timepoint
test_cursor_persist_large_timepoint                           ✅  i64::MAX survives round-trip
test_cursor_overwrite_replaces_previous_value                 ✅  UPSERT semantics confirmed
test_cursor_clear_removes_timepoint                           ✅  clear() → load() = None
test_cursor_clear_on_empty_db_is_non_fatal                    ✅  clear on empty DB doesn't Err
test_cursor_re_persist_after_clear                            ✅  persist→clear→persist cycle works
```

### `crates/ingestor/tests/projector_donates_to.rs` (1 test)

Regression test for the `donates_to` projection skip (debug session `donates-to-projection-skip`). Drives the **corrected** Electoral Commission event shape — a bare `entity_id` donor src plus a `PA:<slug>` PoliticalActor recipient — through the real `Projector` and asserts a single clean `donates_to` relation row materialises. Guards against two defects: (1) record-id-form src ids (`entity:\`GB-COH:NNN\``) that the projector skipped as "unknown src entity", and (2) a doubled `political_actor:political_actor:<slug>` OUT endpoint that broke read-path name resolution. The test also verifies the recipient display name round-trips via the same prefix-strip the entity read path uses.

```
test_project_donates_to_party_recipient_materialises_row      ✅  bare entity_id src + PA:<slug> recipient → exactly 1 donates_to row; single political_actor: prefix; display name resolves
```

---

### `crates/ingestor/tests/planning_integration.rs` (7 mapper contract tests + 1 `URBAN_DB_TESTS`-gated + 2 `URBAN_PLANNING_TESTS` sidecar-gated)

Tests for the planning portal ingestion pipeline: mapper contracts, entity identity keying, and end-to-end sidecar scraping.

**Mapper contract tests (always run — no external services required):**

```
mapper_produces_4_nodes_3_edges                  ✅  PlanningPortalItem → 7 IngestEvents
mapper_source_is_planning_portal                 ✅  Contract source field = "planning_portal"
mapper_contract_id_starts_with_planning_prefix   ✅  Contract EntityId = "planning:{reference}"
mapper_entity_id_starts_with_director_prefix     ✅  Applicant EntityId = "director:{slug}"
mapper_ward_code_on_contract                     ✅  ContractData.ward_code = Some(ward_code)
mapper_skips_item_without_reference              ✅  Missing reference → empty Vec
mapper_handles_empty_applicant_gracefully        ✅  Blank name → provisional node, no panic
```

**Sidecar-gated tests** (require `URBAN_PLANNING_TESTS=1` and the Scrapling sidecar running on port 8765):

```bash
# Start sidecar first:
cd scrapling-sidecar && python sidecar.py

# Then run sidecar-gated tests:
URBAN_PLANNING_TESTS=1 cargo test -p urban-integrity-ingestor --test planning_integration -- --nocapture
```

These tests hit a real Idox-based planning portal (Isle of Wight) and validate the end-to-end scrape → map → ingest pipeline.

> Isle of Wight (IoW) is used as an alternative Idox-format portal for live smoke tests. Anglesey moved to a Salesforce/Arcus portal and is served by a separate sidecar adapter.

---

## Live Integration Tests

### CH / CF API Tests

**File:** `crates/ingestor/tests/live.rs`

Make real HTTP requests to UK government data feeds. Skipped by default unless `URBAN_LIVE_TESTS=1` is set.

#### Prerequisites

| Requirement | Detail |
|-------------|--------|
| `URBAN_LIVE_TESTS=1` | Must be set or all tests skip immediately |
| `URBAN__INGEST__CH_API_KEY` | Required for CH REST tests — free key at [developer.companieshouse.gov.uk](https://developer.companieshouse.gov.uk) |
| Internet access | Tests hit live government endpoints |

```bash
# Run all live tests with output
URBAN_LIVE_TESTS=1 cargo test --test live -p urban-integrity-ingestor -- --nocapture

# Run live tests under llvm-cov
URBAN_LIVE_TESTS=1 cargo llvm-cov --workspace --html

# Run live tests only, summary coverage
URBAN_LIVE_TESTS=1 cargo llvm-cov --test live -p urban-integrity-ingestor --summary-only
```

### Test Inventory

#### Contracts Finder (public REST API — no auth required)

```
live_cf_api_returns_valid_ocds_package            ✅  5 releases received, parsed as OcdsPackage
live_cf_releases_have_required_ocds_fields        ✅  all releases have non-empty ocid + id
live_cf_mapper_produces_graph_mutations_from_real_data  ✅  20 releases → 136 graph mutations
live_cf_award_releases_produce_contract_and_party_nodes ✅  Contract + Entity nodes confirmed
live_cf_pagination_next_link_works                ✅  page 2 contains distinct ocids from page 1
```

#### Companies House REST API (requires `URBAN__INGEST__CH_API_KEY`)

```
live_ch_company_profile_fetch_and_parse           ✅  company_number, company_name, company_status present
live_ch_company_officers_list                     ✅  7 officers returned with name + officer_role
live_ch_rest_data_maps_to_graph_mutation          ✅  real CH data → ChEvent → mapper → pipeline (0 rejections)
```

> **Note on the test fixture:** The CH tests use company number `04713697` (3 call sites in `crates/ingestor/tests/live.rs`), chosen as a stable, long-lived public record. The in-code comments disagree about whose number it is — one says "BBC's Companies House number", the next line says "Ofcom — stable public body" — and the company identity is a live-API fact, not something the repository can settle. <!-- VERIFY: the registered name and status of company 04713697, and which of the two in-code comments is correct -->

### What Live Tests Validate That Stubs Cannot

- **API schema drift** — live tests fail immediately if Companies House or Contracts Finder changes their JSON structure, even if all struct fields are `Option<T>`
- **Pagination contract** — asserts the `links.next` URL actually returns a distinct second page
- **Pipeline acceptance** — real CH events are run through `validate → normalise → to_mutation`; any rejection surfaces a real-world incompatibility
- **Mapper completeness** — 136 mutations from 20 real CF releases confirms the mapper is producing output for the current API response shape

---

## Benchmarks

```bash
# End-to-end Ingest-to-Alert P99 latency
cargo bench -p urban-integrity-core --bench macro_bench

# Motif detector traversal speed on 10k-node CSR snapshot
cargo bench -p urban-integrity-detector --bench motif_detectors

# P50/P99 reporting via example binary
cargo run -p urban-integrity-core --example latency_monitor
```

Benchmarks are **not run in CI** — nothing in `.github/workflows/ci.yml` invokes `cargo bench`, so a
performance regression will not fail a build. The one exception is the concurrent-read latency check, which is additionally
expressed as an ordinary `#[test]` in `crates/core/tests/performance.rs` and therefore *does* run on
every CI build.

### Benchmark Baselines

| Benchmark | Target | Measured | Provenance |
|-----------|--------|---------|------------|
| Ingest-to-Alert P99 | < 500ms | ~3ms | historical; re-run `macro_bench` before quoting <!-- VERIFY --> |
| PhoenixDetector (10k nodes) | < 10ms | sub-1ms | historical <!-- VERIFY --> |
| NepotismDetector (10k nodes) | < 10ms | sub-2ms | historical <!-- VERIFY --> |
| ClusterDetector (10k nodes) | < 10ms | sub-1ms | historical <!-- VERIFY --> |
| 10k concurrent CSR reads P99 | < 10ms | **2.451 µs** | re-measured 2026-08-20 (P50 679 ns, MAX 19.219 µs, over 100,000 nodes) |

Only the last row was re-measured in this pass, because it is the one that runs as a test rather
than as a Criterion benchmark. The four Criterion figures above are carried forward from earlier
runs and are marked as such rather than presented as current.

```bash
# Concurrent-read Criterion benchmark
cargo bench -p urban-integrity-core --bench concurrent_readers

# The same check as a plain test — runs in CI, fails if P99 > 10ms, and prints the numbers
cargo test -p urban-integrity-core --test performance -- --nocapture
```

Reduce benchmark noise with CPU affinity:
```bash
taskset -c 2 cargo bench -p urban-integrity-detector --bench motif_detectors
```

---

## Writing New Tests

### Non-Vacuous Assertions

**"A test exists" is not evidence the guarantee holds.** This document already says a DB-gated test
can pass by returning at line one. The same failure reaches you a second way: a test that runs every
line, asserts something true, and would still pass with the feature deleted.

The worked example is `every_verdict_variant_renders_in_detail_groups_or_the_skipped_section` in
`crates/probe/src/report/mod.rs`. Its job is to prove each of the 7 `Verdict` variants
reaches a prose home in the evidence report — either a `DETAIL_GROUPS` section or `skipped::render`.
It originally asserted:

```rust
assert!(report.contains(&entry.finding.probe));   // vacuous
```

`ledger::render` emits a `| … | {probe} | … |` table row for **every** entry regardless of verdict,
so that containment held for all 7 variants **with the entire `skipped::render` call deleted and
every `DETAIL_GROUPS` row dropped**. The test asserted the ledger existed, and was read for weeks
as asserting the routing worked, until a security audit found it.

The fix asserts on each home's own marker, and on *exactly one* of them:

```rust
// crates/probe/src/report/mod.rs
let detail_home  = format!("### {}\n", entry.finding.probe);
let skipped_home = format!("- {} — ", entry.finding.probe);
let homes = usize::from(report.contains(&detail_home))
          + usize::from(report.contains(&skipped_home));

assert_eq!(homes, 1, "probe {} (verdict {:?}) renders in {homes} prose homes, expected exactly 1 …",
           entry.finding.probe, entry.finding.verdict);
```

`assert_eq!(homes, 1)` rather than `assert!(homes >= 1)` catches the opposite drift too — a variant
acquiring a second home and rendering twice.

**The fix was verified by mutation, not by inspection:** delete the `skipped::render` call, re-run,
confirm the test now fails, restore. That is the only check that distinguishes "the assertion is
tighter" from "the assertion is tighter *and* load-bearing". Reading the new assertion and agreeing
it looks stronger is exactly the step that let the original through.

Practical rules, all of them things this suite has been bitten by:

- **Assert on the specific marker the feature emits**, never on whole-output containment — some
  other renderer almost certainly emits the same token.
- **Prefer exactly-one to at-least-one** where the guarantee is a routing or partitioning one.
- **Mutate to verify.** Break the thing the test guards, watch it go red, put it back. Budget a
  minute for this on any test whose failure mode is "silently always passes".
- **Assert non-vacuity inside scanning tests.** Every source-scan test here
  (`tier1_probes_issue_no_raw_queries`, `probe_report_statics.rs`) asserts the walk saw at least one
  file, so a broken walk cannot pass by inspecting nothing. A scan is the easiest kind of test to
  make vacuous.
- **Test the detector before trusting the scan.** `probe_report_statics.rs` ships a paired test per
  scan that exercises the detection helper against in-test fixtures first.
- **A `cargo test` filter matching zero tests exits 0**, so a stale test name in a checklist reads
  as a pass. Cite the printed `test <name> … ok` line, not the exit code.

### Unit Test Pattern

Place unit tests inside `#[cfg(test)]` blocks in the source file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::graph::storage::GraphStorage;

    #[test]
    fn test_detector_empty_graph() {
        let storage = GraphStorage::new();
        let snapshot = bake_csr(&storage, 0);
        let results = detector.check_tier_b(&snapshot, &[], &HashMap::new());
        assert!(results.is_empty());
    }
}
```

### SurrealDB Integration Test Pattern

Tests in `crates/*/tests/` that need a live SurrealDB use the shared harness. Do **not** hand-roll a
namespace — see [Test Namespace Hygiene](#test-namespace-hygiene).

Add the dev-dependency (`core`, `ingestor`, `enricher`, `cli`, `api`, `probe` and `workbench` already have it):

```toml
[dev-dependencies]
urban-integrity-test-support = { path = "../test-support" }
```

Then:

```rust
// crates/core/tests/my_surreal_test.rs
use urban_integrity_test_support::{db_tests_enabled, TestNs};

// Connects, selects a fresh test_{uuid} namespace + the named database, and
// applies SCHEMA_DEF. Removes the namespace when the returned handle drops.
async fn make_test_db() -> TestNs {
    TestNs::connect("main").await
}

#[tokio::test]
async fn test_my_persistence_feature() {
    if !db_tests_enabled() { return; }   // env gate + TCP pre-flight
    let db = make_test_db().await;       // hold it — dropping it tears the namespace down
    db.query("SELECT * FROM graph_events").await.unwrap();   // reaches Surreal<Any> via Deref
}
```

When the test drives the DB through something other than a handle — a spawned CLI, a config struct,
a second client — take the name only, and keep the guard bound:

```rust
use urban_integrity_test_support::NsGuard;

let ns = NsGuard::with_prefix("test_planning");
let cfg = test_config(ns.name(), None);
// `ns` must live until the end of the test
```

> **Key rules:**
> - Never hand-roll `format!("test_{uuid}")`. Nothing removes it — that pattern stranded 2,889 namespaces in the local store.
> - Keep the guard bound for the whole test: `let (db, _ns) = …`, never `let (db, _) = …`.
> - Always gate on `db_tests_enabled()` so `cargo test --workspace` exits 0 when the DB isn't running.
> - The harness connects over `http://localhost:8000` (not `ws://`) — the SDK (`=3.1.0-beta.2`) connects reliably over HTTP. Override with `URBAN_TEST_DB_URL`.
> - For entity-id lookups, filter Rust-side (fetch rows by `kind` + `weight_json CONTAINS`, then match in Rust) rather than using `payload.entity_id = $id` in WHERE clauses — SurrealDB 3.x coerces `table:key` strings to `RecordId` on write, breaking string equality at query time.
> - Never pair the indexed `kind` equality with an unindexed `CONTAINS` in a production query, and never use a large bare `LIMIT`. Both under-return silently; `crates/core/tests/query_shape_guard.rs` fails the build if you write one. Use `urban_integrity_core::persistence::scan` instead.

### Integration Test Pattern

Add files to `crates/<crate>/tests/`. The integration test for the detector uses `urban-integrity-core` and `urban-integrity-detector` together:

```rust
// crates/detector/tests/my_test.rs
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_core::graph::snapshot::bake_csr;
use urban_integrity_detector::DetectorEngine;

#[test]
fn test_my_motif() {
    let storage = GraphStorage::new();
    // Build the graph...
    let snapshot = bake_csr(&storage, 1);
    // Map NodeIndex → EntityId string
    let mut id_map = HashMap::new();
    for (id, idx, _, _) in storage.snapshot_nodes() {   // 4-tuple!
        id_map.insert(idx.index() as u32, id.0.clone());
    }
    let delta = snapshot.delta_since(0);
    let results = engine.run_tier_b(&snapshot, &delta, &id_map);
    assert_eq!(results.len(), 1);
}
```

> **Note:** `snapshot_nodes()` returns a 4-tuple `(EntityId, NodeIndex, NodeWeight, u64)`. Always destructure with `(id, idx, weight, event_id)` or `(id, idx, _, _)`.

### Adding a Criterion Benchmark

```rust
// crates/detector/benches/my_bench.rs
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_my_detector(c: &mut Criterion) {
    let snapshot = make_10k_snapshot();
    let id_map = make_id_map(&snapshot);
    c.bench_function("my_detector_10k", |b| {
        b.iter(|| detector.check_tier_b(&snapshot, &all_nodes, &id_map))
    });
}

criterion_group!(benches, bench_my_detector);
criterion_main!(benches);
```

Add to `crates/detector/Cargo.toml`:
```toml
[[bench]]
name = "my_bench"
harness = false
```

---

## Known Test Dependencies

- `crates/test-support` (`urban-integrity-test-support`) is a dev-dependency of `core`, `ingestor`, `enricher`, `cli`, `api`, `probe` and `workbench`. It is the only sanctioned source of a test namespace; it depends on `surrealdb`, `tokio`, `uuid`, `urban-integrity-schema` (for `SCHEMA_DEF`), `chrono`, `serde_json` and `anyhow`
- `crates/core/Cargo.toml` `[dev-dependencies]` includes `urban-integrity-detector` for `hybrid_validation.rs`
- `crates/core/Cargo.toml` `[dev-dependencies]` includes `urban-integrity-schema` and `uuid` for `surreal_integration.rs`
- `crates/ingestor/Cargo.toml` `[dependencies]` includes `uuid` (regular dependency); `[dev-dependencies]` includes `urban-integrity-schema` for `cursor_integration.rs` and `projector_donates_to.rs`
- `crates/detector/tests/integration.rs` depends on `urban-integrity-core` and `tempfile`
- `crates/cli/tests/analysis_lifecycle.rs` shells out to the `surreal` binary for its 2 clone tests (resolved via `$SURREAL_BIN`, then `~/.surrealdb/surreal`, then `$PATH`) and skips cleanly without it
- SurrealDB integration tests require SDK `=3.1.0-beta.2` — do not upgrade the SDK without testing the full integration suite first

---

## Suite Growth Since v1.0

The suite has grown substantially since v1.0 (170 base tests — a historical measurement, not
reproducible from the current tree). The sections below document inline
unit tests and integration test files added after the original inventory above.

### `crates/core` — Additional Inline Unit Tests

**`persistence/checkpoint.rs`** — 4 tests (3 new beyond the original roundtrip):

```
test_checkpoint_roundtrip                               ✅  write → hydrate round-trip with 1 node
test_hydrate_from_nonexistent_dir_returns_zero          ✅  no checkpoint file → timestamp 0, empty storage
test_checkpoint_with_ch_timepoint_preserved             ✅  CH timepoint string survives write→hydrate cycle
test_hydrate_skips_malformed_node_weight_and_continues  ✅  bad JSON node skipped, valid node still loaded
```

**`persistence/surreal.rs`** — 12 inline unit tests:

```
test_batch_controller_initial_size                      ✅  default size = 50
test_batch_controller_increases_on_high_latency         ✅  >200ms latency → size increases (capped at max)
test_batch_controller_decreases_on_low_latency          ✅  <50ms latency → size decreases (floored at min)
test_batch_controller_clamps_to_min                     ✅  repeated low-latency shrinks stop at min=10
test_batch_controller_clamps_to_max                     ✅  repeated high-latency grows stop at max=500
test_batch_controller_at_target_latency_increases       ✅  latency at boundary → size increases
test_surreal_writer_config_defaults                     ✅  default config fields match expected values
test_event_record_from_node_added                       ✅  NodeAdded WAL event → EventRecord kind="node_added"
test_event_record_from_edge_added                       ✅  EdgeAdded WAL event → EventRecord kind="edge_added"
test_event_record_from_node_removed                     ✅  NodeRemoved WAL event → EventRecord kind="node_removed"
test_event_record_from_motif_detected                   ✅  MotifDetected WAL event → EventRecord kind="motif_detected"
test_event_record_is_serializable                       ✅  EventRecord serializes/deserializes via serde_json
```

**`persistence/queries.rs`** — 86 inline unit tests; the 30 below are the load-bearing ones (the 12 most recent, covering alert ordering and truncation, are listed under *Alert read path* further down):

```
test_parse_alert_row_phoenix                            ✅  motif_detected row → AlertRecord with phoenix type
test_parse_alert_row_no_ward                            ✅  row without ward_code → ward_code = None
test_parse_alert_row_wrong_kind                         ✅  non-motif row kind → returns None
test_parse_alert_row_malformed_result_json             ✅  bad result_json field → returns None
test_filter_by_score                                    ✅  score filter drops rows below threshold
test_filter_by_ward                                     ✅  ward filter drops rows with wrong ward code
test_alert_filter_default                               ✅  AlertFilter default has min_score=0.0, no ward
test_parse_entity_node_row_returns_none_for_motif_event ✅  motif_detected kind → None for entity parse
test_parse_entity_node_row_returns_some_for_node_added_event ✅  node_added kind with Entity weight → Some
test_is_slug_shape_detects_hyphens                      ✅  "director:john-smith" → is_slug=true
test_is_slug_shape_detects_colons                       ✅  "GB-COH:12345678" → is_slug=true
test_is_slug_shape_plain_name_returns_false             ✅  "Apex Build Ltd" → is_slug=false
test_is_valid_ch_number_accepts_digits                  ✅  "12345678" and "SC123456" accepted
test_is_valid_ch_number_rejects_special_chars           ✅  injection chars and spaces rejected
test_parse_alert_row_ward_code_in_score_breakdown       ✅  ward_code extracted from score_breakdown JSON
test_query_alerts_respects_from_date                    ✅  from_date filter excludes older events
test_query_alerts_unlimited                             ✅  limit=0 returns all seeded events
test_enrich_alert_participants_empty                    ✅  empty participants map → no enrichment panic
test_is_valid_event_id_accepts_safe_ids                 ✅  UUID-format IDs accepted
test_is_valid_event_id_rejects_injection_chars          ✅  SQL-injection chars rejected
test_is_valid_event_id_rejects_empty_and_overlong       ✅  empty string and >128-char IDs rejected
test_parse_officer_node_row_returns_some_for_officer    ✅  node_added with Officer weight → Some
test_parse_officer_node_row_returns_none_for_entity_node ✅  Entity-weight node → None for officer parse
test_parse_officer_node_row_returns_none_for_motif_row  ✅  motif_detected kind → None
test_parse_officer_node_row_returns_none_for_missing_weight_json ✅  missing weight_json field → None
test_parse_entity_node_row_returns_none_for_officer_node ✅  Officer-weight node → None for entity parse
test_parse_entity_node_row_returns_none_for_missing_weight_json  ✅  missing weight_json → None
test_is_slug_shape_empty_string_returns_false           ✅  empty string → false
test_is_slug_shape_whitespace_trims_before_check        ✅  leading/trailing whitespace trimmed
test_is_slug_shape_director_slug_format                 ✅  "director:jane-doe" → is_slug=true
```

### `crates/core` — New Integration Test File

**`crates/core/tests/contract_row_fields.rs`** — 2 active tests + 1 DB-gated (skip-by-return, not `#[ignore]`):

```
rep_04_contract_row_carries_tender_ref      ✅  ContractRow.tender_ref = Some("CF-TEST") round-trips
rep_04_contract_row_carries_application_ref ✅  ContractRow.application_ref = Some("FUL/2024/001") round-trips
rep_04_assemble_entity_profile_copies_contract_refs  ⏭  DB-gated — returns early unless URBAN_DB_TESTS=1
```

### `crates/detector` — Additional Inline Unit Tests

**`alert/cache.rs`** — 5 new tests beyond `test_alert_cache_eviction`:

```
test_alert_cache_is_empty_on_new_cache      ✅  new cache → is_empty()=true, len()=0
test_alert_cache_get_returns_inserted_item  ✅  insert(42) → get(42) returns the same item
test_alert_cache_get_returns_none_for_missing_key  ✅  get(999) on empty cache → None
test_alert_cache_contains_after_insert      ✅  contains(7)=false → insert(7) → contains(7)=true
test_alert_cache_len_increases_with_inserts ✅  insert×2 → len()=2, is_empty()=false
```

### `crates/ingestor` — Additional Inline Unit Tests

**`ch/client.rs`** — 8 inline unit tests:

```
test_inmemory_cursor_store_default_is_none        ✅  default InMemoryCursorStore → load_timepoint()=None
test_inmemory_cursor_store_persist_and_load       ✅  persist(42) → load()=Some(42)
test_inmemory_cursor_store_clear_resets_to_none   ✅  persist→clear → load()=None
test_connect_and_stream_returns_err_on_unreachable ✅  unreachable host → Err (no panic)
test_connect_and_stream_with_timepoint_in_url     ✅  stored timepoint appended to request URL; Err on unreachable
test_connect_and_stream_clears_cursor_on_416_response ✅  mock 416 response → cursor cleared, Ok returned
test_connect_and_stream_parses_ndjson_events      ✅  mock 200 NDJSON response → valid events ingested, bad lines skipped
the_officer_client_subscribes_to_a_different_stream_than_the_company_client ✅  new_officers() targets /officers, new() targets /companies — the two stream_base values must differ
```

**`cf/client.rs`** — 6 inline unit tests:

```
test_inmemory_cf_cursor_default_is_none                          ✅  default cursor → load_cursor()=None
test_inmemory_cf_cursor_persist_and_load                         ✅  persist("2026-01-15") → load()=Some("2026-01-15")
test_cfpoller_poll_once_returns_err_on_unreachable_server        ✅  unreachable host → Err
test_cfpoller_poll_once_empty_releases_via_mock_server           ✅  mock {"releases":[]} → Ok, no mutations
test_cfpoller_poll_once_with_cursor_advances_url                 ✅  stored cursor date appended to request URL
test_cfpoller_poll_once_with_single_release_via_mock_server      ✅  mock with 1 release → processed without panic
```

**`planning/poller.rs`** — 3 new inline unit tests:

```
test_planning_poller_new_builds_correctly                   ✅  PlanningPoller constructs with 0 portals, correct interval
test_poll_once_with_unreachable_sidecar_returns_gracefully  ✅  unreachable sidecar → WARN log, no panic
test_poll_once_with_mock_sidecar_items_pushes_mutations     ✅  mock sidecar with 1 item → mutations pushed to coordinator
```

**`planning/sidecar.rs`** — 13 inline unit tests:

```
test_sidecar_new_builds_correct_base_url        ✅  ScrapingSidecar URL assembled from host + port
test_scrape_response_deserialization_ok         ✅  valid JSON response deserializes to ScrapeResponse
test_scrape_response_deserialization_error      ✅  error-status JSON deserializes to ScrapeResponse{results:[]}
test_scrape_returns_empty_on_unreachable_sidecar ✅  unreachable sidecar → empty Vec, no panic
test_health_returns_false_on_unreachable_sidecar ✅  unreachable sidecar → health()=false
test_scrape_returns_items_from_mock_ok_response  ✅  mock 200 response → Vec<PlanningPortalItem> with correct fields
test_scrape_returns_empty_on_error_status_response ✅  mock {"status":"error"} → empty Vec
test_scrape_returns_empty_on_malformed_json_response ✅  mock non-JSON response → empty Vec
test_scrape_handles_ward_and_applicant_fields    ✅  ward and applicant fields parsed from mock response
test_with_timeout_records_the_configured_ceiling ✅  with_timeout() retains the configured seconds; new() falls back to DEFAULT_TIMEOUT_SECS
test_scrape_checked_reports_transport_failure_not_empty_items ✅  unreachable sidecar → ScrapeOutcome::Failed(Transport), not an empty item list
test_scrape_checked_reports_ok_with_zero_results_as_items ✅  {"status":"ok","results":[]} → Items(vec![]) — a real answer, not a failure
test_scrape_checked_reports_error_status_and_malformed_distinctly ✅  error status → Failed(ErrorStatus); non-JSON body → Failed(MalformedResponse)
```

**`cf/mapper.rs`** — 9 inline unit tests:

```
test_map_release_produces_contract_node         ✅  OCDS release → Contract node in IngestEvents
test_map_release_produces_supplier_awarded_to_edge ✅  supplier party → AwardedTo edge
test_map_release_buyer_gets_issued_by_edge      ✅  buyer party → IssuedBy edge
test_party_entity_id_prefers_gb_coh             ✅  GB-COH scheme identifier used as EntityId
test_party_entity_id_falls_back_to_cf_party     ✅  no GB-COH → cf-party:{id} fallback
test_map_release_populates_tender_ref           ✅  tender.id → ContractData.tender_ref
test_map_release_no_tender_yields_none          ✅  release without tender → tender_ref=None
date_published_falls_back_to_release_date       ✅  no tender.datePublished → ContractData.date_published from release.date
tender_date_published_wins_over_release_date    ✅  both present → tender.datePublished takes precedence
```

**`ch/mapper.rs`** — 8 new inline unit tests:

```
test_map_company_event_produces_entity_node     ✅  company-profile ChEvent → Entity node
test_map_company_event_produces_location_and_edge ✅  registered_office_address → Location node + LocatedAt edge
test_map_officer_event_produces_officer_and_directs_edge ✅  officers ChEvent → Officer node + Directs edge
test_unknown_resource_kind_returns_empty        ✅  unrecognised resource_kind → empty Vec
test_map_entity_status_dissolved_with_date      ✅  "dissolved" status → EntityStatus::Dissolved with date
test_map_entity_status_active                   ✅  "active" status → EntityStatus::Active
test_map_entity_status_none_defaults_active     ✅  None status → EntityStatus::Active (default)
test_ch_mapper_captures_date_of_creation        ✅  date_of_creation copied to EntityData.incorporation_date
```

**`planning/mapper.rs`** — 22 inline unit tests; the 8 below are the contract-defining ones:

```
test_map_planning_item_produces_seven_events          ✅  one PlanningPortalItem → 7 IngestEvents
test_map_planning_item_contract_has_ward_code         ✅  ward string → ward_code on ContractData
test_map_planning_item_entity_is_provisional          ✅  applicant node has EntityStatus::Provisional
test_map_planning_item_entity_key_is_not_application_scoped ✅  applicant EntityId uses global director slug
test_map_planning_item_missing_reference_returns_empty ✅  blank reference → empty Vec, no panic
test_extract_postcode_finds_valid_postcode             ✅  "B1 1AA" extracted from address string
test_extract_postcode_returns_none_when_absent        ✅  address without postcode pattern → None
test_map_planning_item_populates_application_ref      ✅  reference field → ContractData.application_ref
```

**`address/mod.rs`** — 15 inline unit tests:

```
test_normalise_postcode_strips_space            ✅  "B1 1AA" → "B11AA"
test_normalise_postcode_uppercases              ✅  "b11aa" → "B11AA"
test_normalise_postcode_strips_hyphen           ✅  "B1-1AA" → "B11AA"
test_normalise_postcode_rejects_empty           ✅  empty string → None
test_normalise_postcode_rejects_too_short       ✅  "AB" → None (too short)
test_normalise_postcode_rejects_special_chars   ✅  special characters → None
test_extract_numeric_house_number               ✅  "42 High Street" → "42"
test_extract_alphanumeric_house_number          ✅  "14A Low Road" → "14A"
test_extract_named_building_returns_empty       ✅  "The Old Mill" → ""
test_extract_empty_street_returns_empty         ✅  empty string → ""
test_normalise_address_produces_canonical_entity_id ✅  address + postcode → deterministic EntityId
test_normalise_address_same_address_same_entity_id  ✅  identical inputs → identical EntityId
test_normalise_address_none_postcode_returns_none   ✅  None postcode → None
test_normalise_address_different_postcodes_produce_different_ids ✅  different postcodes → different IDs
test_normalise_address_without_house_number_is_postcode_only    ✅  unnamed building → postcode-only key
```

**`projector.rs`** — 16 new inline unit tests (projection layer, pure functions, no DB):

```
test_table_entity                               ✅  NodeWeight::Entity → "entity"
test_table_officer                              ✅  NodeWeight::Officer → "officer"
test_table_location                             ✅  NodeWeight::Location → "location"
test_table_contract                             ✅  NodeWeight::Contract → "contract"
test_table_ward                                 ✅  NodeWeight::Ward → "ward"
test_table_political_actor                      ✅  NodeWeight::PoliticalActor → "political_actor"
test_node_data_entity_has_entity_id             ✅  Entity node_data has entity_id, name, company_number, entity_status
test_node_data_officer_fields                   ✅  Officer node_data has entity_id, officer_role, appointed_on, resigned_on
test_node_data_contract_fields                  ✅  Contract node_data has ocid, value_gbp, ward_code
test_edge_rel_table_directs                     ✅  EdgeWeight::Directs → "directs"
test_edge_rel_table_located_at                  ✅  EdgeWeight::LocatedAt → "located_at"
test_edge_rel_table_awarded_to                  ✅  EdgeWeight::AwardedTo → "awarded_to"
test_edge_rel_table_issued_by                   ✅  EdgeWeight::IssuedBy → "issued_by"
test_edge_rel_table_represents                  ✅  EdgeWeight::Represents → "represents"
test_edge_data_directs_has_officer_role         ✅  Directs edge_data has data.details, data.officer_role, meta.created_at
test_edge_data_awarded_to                       ✅  AwardedTo edge_data has data.details
```

**`backfill/ch.rs`** — 13 inline unit tests (CH backfill name matching, no DB):

> **Exact-match-only (2026-06-13, commit 43675f8):** the former edit-distance (≤2)
> `MatchVerdict::FuzzyWarn` path and its `edit_distance` helper were removed. `match_company`
> now returns only `Exact`, `Ambiguous`, or `NoMatch` — a near-miss name is treated as
> `NoMatch` and no `company_number` is assigned on a non-exact name. The previous
> `test_edit_distance_*` and `test_match_fuzzy_*` tests no longer exist.

```
test_normalise_strips_ltd                       ✅  "ACME Ltd" → "acme"
test_normalise_strips_limited                   ✅  "Apex Build Limited" → "apex build"
test_normalise_strips_plc                       ✅  "Big Corp Plc" → "big corp"
test_normalise_strips_punctuation               ✅  "O'Brien Construction Ltd." → "obrien construction"
test_normalise_collapses_whitespace             ✅  "  A  B  Ltd  " → "a b"
test_match_exact                                ✅  normalised exact match → MatchVerdict::Exact(number)
test_match_no_results                           ✅  empty results slice → MatchVerdict::NoMatch
test_match_ambiguous_two_exact                  ✅  two identical normalised names → MatchVerdict::Ambiguous
test_match_near_miss_is_no_match_exact_only      ✅  near-miss name (edit distance 1) → MatchVerdict::NoMatch (exact-only policy)
test_match_completely_different_is_no_match      ✅  unrelated name → MatchVerdict::NoMatch
test_match_empty_normalised_name               ✅  query normalises to empty string → MatchVerdict::NoMatch
test_ch_rest_client_returns_items               ✅  mock HTTP 200 with JSON items → parsed Vec<ChSearchItem>
test_ch_rest_client_handles_429                 ✅  mock HTTP 429 → Err returned (not panic)
```

### `crates/api` — New Inline Unit Tests

**`server.rs`** — 5 new inline unit tests:

```
test_server_config_default_values          ✅  bind_addr="127.0.0.1:8080", ollama_url contains "11434"
test_graphiql_handler_returns_html         ✅  graphiql_handler() → HTTP 200
test_health_ext_returns_ok_status          ✅  health_ext() → status="ok", node_count=0
test_metrics_ext_returns_zero_on_empty_graph ✅  metrics_ext() → node_count=0, edge_count=0, last_snapshot_event=0
test_graphiql_handler_returns_html_body    ✅  graphiql_handler() response body status = 200
```

**`http/health.rs`** — 2 new inline unit tests:

```
test_health_handler_returns_ok_status            ✅  health_handler() → status="ok", version non-empty, counts=0
test_metrics_handler_returns_zero_on_empty_graph ✅  metrics_handler() → node_count=0, edge_count=0, last_snapshot_event=0
```

**`grpc/integrity.rs`** — 3 new inline unit tests:

```
test_get_graph_stats_returns_zero_on_empty_graph                ✅  GetGraphStats → node_count=0, edge_count=0
test_query_alerts_returns_invalid_argument_when_limit_over_1000 ✅  limit=1001 → tonic::Code::InvalidArgument
test_build_grpc_routes_does_not_panic                           ✅  build_grpc_routes() completes without panic
```

### `crates/cli` — New Inline Unit Tests and Integration Test Files

**`src/commands/alerts.rs`** — 19 inline unit tests:

```
test_parse_since_hours                              ✅  "24h" → Utc::now() - 24h (±5s)
test_parse_since_days                               ✅  "7d" → Utc::now() - 7d (±5s)
test_parse_since_invalid                            ✅  "invalid", "", "5m", "24H" all return Err
test_alerts_since_conflicts_with_from               ✅  clap rejects --since + --from together
test_slug_substitution_replaces_known_slugs         ✅  director slug and GB-COH slug replaced in text
test_slug_substitution_passthrough_on_no_match      ✅  text with no slugs returned unchanged
test_parse_since_negative_hours_is_err              ✅  "-5h" → Err with "positive" in message
test_parse_since_negative_days_is_err               ✅  "-3d" → Err with "positive" in message
test_parse_since_zero_hours_is_ok                   ✅  "0h" → Ok (non-negative accepted)
test_slug_substitution_longest_key_wins_on_prefix_overlap ✅  "director:john-smith" replaced before "director:john"
test_slug_substitution_ch_number_prefix_overlap     ✅  "GB-COH:12345678" replaced before "GB-COH:1234567"
test_normalize_motif_type_short_names_map_to_internal_variants ✅  "cluster"→AddressCluster, "phoenix"→PhoenixLoop, "nepotism"→NepotismBridge
test_normalize_motif_type_full_variant_names_pass_through_matching ✅  internal variant names accepted case-insensitively
test_normalize_motif_type_unknown_input_passes_through_unchanged ✅  typo ("clustre") passes through unchanged, never coerced to another motif
```

**`src/commands/status.rs`** — 11 inline unit tests; the 3 below cover serialisation and format rejection:

```
test_engine_status_serializes_all_fields                          ✅  EngineStatus JSON contains all 4 fields with correct values
markdown_output_is_rejected_before_any_connection_is_attempted    ✅  run(Markdown) → Err, no DB connection attempted
pdf_output_is_rejected_before_any_connection_is_attempted         ✅  run(Pdf) → Err, no DB connection attempted
```

**`src/commands/entity.rs`** — 9 new inline unit tests:

```
test_is_valid_ch_number_accepts_valid              ✅  "12345678", "SC123456", "NI123456", "OC123456" accepted
test_is_valid_ch_number_rejects_invalid            ✅  empty, too-short, too-long, injection chars rejected
test_render_results_zero_matches_ok                ✅  render_results([], Json) → Ok
test_render_results_single_match_json_ok           ✅  render_results([1 profile], Json) → Ok
test_render_results_multiple_matches_json_ok       ✅  render_results([2 profiles], Json) disambiguation → Ok
test_render_results_multiple_matches_markdown_errors ✅  render_results([2 profiles], Markdown) → Err mentioning "report command"
test_render_single_profile_markdown_errors         ✅  render_single_profile(Markdown) → Err mentioning "report command"
test_render_single_profile_pdf_errors              ✅  render_single_profile(Pdf) → Err
test_render_single_profile_json_ok                 ✅  render_single_profile(Json) → Ok
```

**`src/commands/graph_path.rs`** — 2 inline unit tests (rendering only):

```
render_path_owns_property_edge ✅  OwnsProperty step renders with the property label
render_path_labels             ✅  edge type uppercased as "DIRECTS", node labels + → arrows present
```

The BFS traversal tests are **not** in the CLI crate — they live in
`crates/core/src/persistence/traversal.rs` (`urban-integrity-core`):

```
bfs_finds_shortest_path    ✅  3-node chain A→B→C → path [A,B,C] found
bfs_no_path                ✅  disconnected graph → BFS returns None
bfs_respects_hop_cap       ✅  7-hop chain exceeds MAX_HOPS=6 → None
bfs_undirected             ✅  adjacency builder inserts reverse edge → B→A path found
```

**`src/commands/report.rs`** — 25 inline unit tests:

```
default_filename_ward_scope                          ✅  ward filter → "integrity-report-w05001492-*.md"
default_filename_entity_scope                        ✅  entity name → "integrity-report-apex-build-ltd-*.pdf"
default_filename_all_scope                           ✅  no filter → "integrity-report-all-*"
validate_output_file_format_rejects_json_with_file   ✅  Json + output_file → Err with correct message
validate_output_file_format_accepts_markdown_with_file ✅  Markdown + output_file → Ok
validate_output_file_format_accepts_markdown_without_file ✅  Markdown + None → Ok
validate_output_file_format_rejects_table_with_file  ✅  Table + output_file → Err
render_alert_table_md_header_and_separator           ✅  header row and separator present in output
render_alert_table_md_formats_score_to_3_decimals    ✅  0.876543 rounds to "0.877"
render_alert_table_md_uses_em_dash_for_none_ward     ✅  None ward renders as em-dash (U+2014)
render_markdown_report_contains_all_six_sections     ✅  all 6 required ## headings present
render_markdown_report_no_summary_narrative_when_none ✅  None narrative → section absent
render_markdown_report_includes_summary_narrative_when_some ✅  Some(text) → ## Summary Narrative + text
render_citations_md_ch_hyperlink                     ✅  CH hyperlink with correct URL
render_citations_md_tender_ref_present               ✅  "Contract ref: CF-TEST-001" in output
render_citations_md_application_ref_present          ✅  "Planning application: FUL/2024/001234" in output
render_citations_md_fallback_to_contract_name        ✅  no refs → "Contract: Road Repair Contract" fallback
render_entity_snapshot_md_ch_hyperlink               ✅  CH URL in entity snapshot table
invoke_pdf_tool_with_nonexistent_md_file_returns_result ✅  does not panic; Err mentions pandoc/PDF
render_entity_snapshot_md_no_ch_number_uses_em_dash  ✅  no CH number → em-dash, no hyperlink
render_entity_snapshot_md_no_alerts_shows_em_dash_for_highest ✅  empty alerts → em-dash in Highest Alert cell
render_markdown_report_shows_filter_values_in_header  ✅  ward, min_score, motif, from, to appear in header
render_markdown_report_counts_match_report_fields    ✅  "Total alerts: 3" and "High-risk count: 2" present
render_entity_snapshot_md_highest_alert              ✅  highest-score alert type and score formatted correctly
```

**`tests/report_md.rs`** — 4 active tests + 3 `#[ignore]`:

```
rep_01_render_markdown_full_sections        ✅  all 6 sections present; Summary Narrative absent when None
rep_04_citations_section_ch_hyperlink       ✅  Companies House hyperlink in citations
rep_04_citations_tender_ref_present         ✅  "Contract ref: CF-TEST-001" in citations
rep_04_citations_application_ref_present    ✅  "Planning application: FUL/2024/001234" in citations
rep_02_output_file_writes_to_disk           ⏭  #[ignore] — filesystem write, requires human-verify checkpoint
rep_02_default_filename_format              ⏭  #[ignore] — covered by in-module unit tests
rep_05_entity_scoped_report_resolves_by_name ⏭  #[ignore] — requires SurrealDB
```

**`tests/report_pdf.rs`** — 1 active test + 1 `#[ignore]`:

```
rep_03_invoke_pdf_calls_pandoc_with_args_array  ✅  pandoc present → PDF produced; pandoc absent → Err with an explanatory message, .md intact
rep_03_missing_pandoc_writes_md_then_errors     ⏭  #[ignore] — PATH mutation, run with --test-threads=1
```

**`src/commands/export_graphml.rs`** — no unit tests.

`export_graphml` has no `#[cfg(test)]` block. It is a thin integration layer: it issues `SELECT * FROM {table}` queries against SurrealDB via `connect_db`, assembles a petgraph `DiGraph` in memory, and serialises the result to GraphML XML via a pure `render_graphml` helper. Correctness is verified end-to-end by running the `export-graphml` subcommand against a populated database (`cargo run -p urban-integrity-cli -- export-graphml`). The pure helper functions (`scalar_str`, `scalar_fields`, `xml_esc`) are candidates for future unit tests if the XML rendering logic grows in complexity.

### `crates/schema` — New Inline Unit Tests

**`src/types/mod.rs`** — 2 new inline unit tests:

```
test_node_serialization  ✅  NodeWeight serializes/deserializes via serde_json
test_edge_serialization  ✅  EdgeWeight serializes/deserializes via serde_json
```

**`src/types/node.rs`** — 22 inline unit tests; the 8 below cover the status/serde contract:

```
test_entity_status_default_is_active         ✅  EntityStatus::default() == Active
test_legacy_status_active                    ✅  "active" string → EntityStatus::Active via legacy mapping
test_legacy_status_dissolved                 ✅  "dissolved" string → EntityStatus::Dissolved
test_legacy_status_liquidation               ✅  "liquidation" string → EntityStatus::Dissolved
test_entity_status_serde_roundtrip_active    ✅  Active serializes → "Active", deserializes back
test_entity_status_serde_roundtrip_dissolved ✅  Dissolved serializes → "Dissolved", deserializes back
test_node_weight_entity_roundtrip            ✅  NodeWeight::Entity serde round-trip
test_node_weight_officer_roundtrip           ✅  NodeWeight::Officer serde round-trip
```

### `crates/enricher` Tests

All of the enricher's base tests run without any external services.

**`crates/enricher/tests/hot_path.rs`** — 6 integration tests (compliance):

```
test_hot_path_handle_event_under_5ms   ✅  handle_event P99 < 5ms over 1000 calls — regression fence for hot-path contamination
test_candidate_link_no_mutation        ✅  ConfidenceScore(0.72) → ConfirmedLink::try_new returns None; score 0.85 → Some (boundary verified)
test_enrichment_stats_default_zeroed   ✅  EnrichmentStats::default() → candidates_found/auto_linked/pending_review/discarded all 0
test_enrichment_stats_accumulate       ✅  two stats structs summed → 15 candidates, 6 auto_linked, 5 pending_review, 4 discarded
```

**`crates/enricher/src/confidence.rs`** — inline unit tests (no DB):

```
test_confidence_tier_model_out_of_range_high    ✅  ConfidenceScore::new(1.1) → Err
test_confidence_tier_model_out_of_range_low     ✅  ConfidenceScore::new(-0.01) → Err
test_confidence_tier_model_valid                ✅  ConfidenceScore::new(0.5) → Ok(0.5)
test_confidence_tier_model_zero_boundary        ✅  ConfidenceScore::new(0.0) → Ok
test_confidence_tier_model_one_boundary         ✅  ConfidenceScore::new(1.0) → Ok
test_confirmed_link_boundary_below_threshold    ✅  score 0.84 → ConfirmedLink::try_new returns None (below AUTO_LINK_THRESHOLD 0.85)
test_confirmed_link_boundary_at_threshold       ✅  score 0.85 → ConfirmedLink::try_new returns Some (at threshold)
test_confirmed_link_boundary_above_threshold    ✅  score 1.0 → ConfirmedLink::try_new returns Some
test_candidate_link_boundary_below_threshold    ✅  score 0.69 → CandidateLink::try_new returns None (below CANDIDATE_THRESHOLD 0.70)
test_candidate_link_boundary_at_threshold       ✅  score 0.70 → CandidateLink::try_new returns Some (at threshold)
test_link_status_serialization_auto_linked      ✅  LinkStatus::AutoLinked → "auto_linked"
test_link_status_serialization_pending_review   ✅  LinkStatus::PendingReview → "pending_review"
test_link_status_serialization_confirmed        ✅  LinkStatus::Confirmed → "confirmed"
test_link_status_serialization_rejected         ✅  LinkStatus::Rejected → "rejected"
test_link_status_serialization_candidate        ✅  LinkStatus::Candidate → "candidate"
test_link_status_str_all_variants               ✅  link_status_str() → plain snake_case for all six variants, sub_candidate included
test_link_status_str_differs_from_json_serialization ✅  regression: serde_json emits `"auto_linked"` with quotes; link_status_str must be the unquoted form (SurrealDB double-quote bug)
test_link_status_str_sub_candidate_returns_plain_string ✅  SubCandidate → "sub_candidate", never JSON-quoted
test_link_status_sub_candidate_serde_json_has_quotes ✅  serde produces `"sub_candidate"` — pins the two-representation difference
test_sub_candidate_threshold_boundary_at_050    ✅  score 0.50 → SubCandidateLink::try_new returns Some (SUB_CANDIDATE_THRESHOLD 0.50 is inclusive)
test_sub_candidate_below_threshold_returns_none ✅  score 0.499 → None (the adapter discards it as noise)
```

**DB-gated enricher tests** (`URBAN_DB_TESTS=1`): verify the `identity_links` schema DDL applies cleanly on connect.

**`crates/enricher/src/engine.rs`** — 1 inline unit test (no DB):

```
build_run_marker_upsert_shape   ✅  build_run_marker_upsert("ch_psc", ...) SQL targets enrichment_runs:ch_psc, includes candidates_found/auto_linked/pending_review/discarded but NOT sub_candidate
```

### Running Isolated Subsets

```bash
# Projection layer unit tests (no DB)
cargo test -p urban-integrity-ingestor projector

# Backfill exact name-matching unit tests (no DB)
cargo test -p urban-integrity-ingestor backfill::ch

# Inline unit tests, by module
cargo test -p urban-integrity-core persistence::checkpoint
cargo test -p urban-integrity-core persistence::surreal
cargo test -p urban-integrity-core persistence::queries
cargo test -p urban-integrity-detector alert::cache
cargo test -p urban-integrity-ingestor ch::client
cargo test -p urban-integrity-ingestor cf::client
cargo test -p urban-integrity-ingestor planning::poller
cargo test -p urban-integrity-ingestor planning::sidecar
cargo test -p urban-integrity-ingestor cf::mapper
cargo test -p urban-integrity-ingestor ch::mapper
cargo test -p urban-integrity-ingestor planning::mapper
cargo test -p urban-integrity-ingestor address
cargo test -p urban-integrity-api
cargo test -p urban-integrity-cli

# Enricher crate tests (no DB required)
cargo test -p urban-integrity-enricher

# New integration test files
cargo test --test contract_row_fields -p urban-integrity-core
cargo test --test coordinator_update_node -p urban-integrity-core
cargo test --test report_md -p urban-integrity-cli
cargo test --test report_pdf -p urban-integrity-cli

# graph_path render tests (CLI) and the BFS traversal tests (core — a different crate)
cargo test -p urban-integrity-cli commands::graph_path
cargo test -p urban-integrity-core persistence::traversal

# Run the PDF report test in isolation to avoid PATH mutation races
cargo test --test report_pdf -p urban-integrity-cli rep_03_missing_pandoc_writes_md_then_errors -- --test-threads=1 --ignored
```

### Growth: OSINT Adapters, Evasion Detector and `verify`

The CH PSC and Disqualified Director adapters, the Evasion Detector, the OSINT enrichment surface and the `verify` CLI command landed together. Their tests span both inline unit tests and dedicated integration test files.

#### Inline unit tests

**`crates/enricher/src/adapters/ch_psc.rs`** — 40 inline unit tests (five are DB-gated):

```
test_extract_company_number_from_resource_uri     ✅  "/company/12345678/persons-with-significant-control/..." → "12345678"
test_extract_company_number_sc_prefix             ✅  SC-prefixed number extracted from resource_uri
test_extract_company_number_falls_back_to_links_self ✅  empty resource_uri → number taken from links.self
test_extract_company_number_invalid_path          ✅  "/invalid/path" → None
test_skip_corporate_entity_kind                   ✅  corporate-entity PSC kind → true (skip)
test_skip_legal_person_kind                       ✅  legal-person PSC kind → true (skip)
test_individual_kind_not_skipped                  ✅  individual-person kind → false (do not skip)
test_valid_ch_number_digits                       ✅  "12345678" → true
test_valid_ch_number_sc_prefix                    ✅  "SC123456" → true
test_valid_ch_number_ni_prefix                    ✅  "NI123456" → true
test_invalid_ch_number_special_chars              ✅  "12345678; DELETE", "'quoted'" and "" → false
test_normalise_john_smith                         ✅  name normalisation for the director slug
test_strip_title_mr / _ms / _no_prefix            ✅  (3 tests) honorific stripped; untitled names unchanged
test_build_identity_link_off_composite            ✅  OFF composite match → identity_link fields
test_build_identity_link_director_slug            ✅  director-slug fallback → identity_link fields
test_psc_adapter_disabled_when_no_key / _empty_key / _whitespace_key ✅  (3 tests) is_enabled()=false
test_psc_adapter_enabled_with_real_key            ✅  is_enabled()=true
test_psc_adapter_name                             ✅  adapter.name() == "ch_psc"
test_resolve_display_name_prefers_name_elements   ✅  name_elements win over the flat name field
test_resolve_display_name_falls_back_to_name      ✅  no name_elements → flat name used
test_resolve_display_name_strips_mr_title_in_fallback ✅  honorific stripped on the fallback path
test_resolve_display_name_empty_name_elements_falls_back ✅  empty name_elements → flat name used
officer_slugs_cover_the_ch_surname_first_convention ✅  "SMITH, John" produces both slug orders
officer_slugs_leave_a_comma_free_name_alone       ✅  comma-free name → single slug
officer_slugs_handle_a_trailing_honorific         ✅  trailing honorific dropped from the slug
officer_slugs_reject_a_dangling_comma             ✅  malformed "NAME," produces no bogus slug
officer_company_number_falls_back_to_the_entity_id ✅  company number parsed out of the officer EntityId
test_build_officer_id_set_hyphen_slug_regression  ✅  DB-gated: officer id set built from payload.weight_json (not entity_id)
test_build_officer_id_set_includes_off_composite  ✅  DB-gated: OFF composite ids included in the set
test_fetch_company_numbers_deduplicates           ✅  DB-gated: duplicate company numbers collapsed
test_write_identity_link_status_stored_as_plain_string ✅  DB-gated: status persisted as a plain string
test_write_identity_link_idempotent               ✅  DB-gated: re-running the same write does not duplicate the link
```

**`crates/enricher/src/adapters/ch_disqualified.rs`** — 24 inline unit tests (the last five are DB-gated); the confidence and DOB contract:

```
test_parse_dob_full_month_year                    ✅  month+year DOB parsed (CH publishes no day)
test_parse_dob_year_only                          ✅  "1985" → ParsedDob{year, month: None}
test_parse_dob_invalid                            ✅  "not-a-date" → None
test_parse_dob_december                           ✅  December boundary parses correctly
disq_confidence_full_month_year_is_095            ✅  full DOB + name + company → 0.95
disq_confidence_year_only_is_090                  ✅  year-only DOB + name + company → 0.90
disq_confidence_name_only_is_none                 ✅  name only → None (three-field-minimum invariant)
disq_confidence_name_not_matched_is_none          ✅  name_matched=false → None regardless of DOB
test_fetch_officer_names_uses_weight_json_not_entity_id ✅  DB-gated: officer lookup reads payload.weight_json, not entity_id CONTAINS
```

**`crates/enricher/src/evasion.rs`** — inline unit tests:

```
compute_motif_hash_is_deterministic               ✅  same officer_id + case_number → identical hash
an_already_alerted_hash_suppresses_a_re_emit      ✅  a hash already in AlertedHashes is recognised; an unrelated hash is not
an_empty_set_re_emits_rather_than_suppresses      ✅  a load error yields an empty set, so candidates re-emit — a duplicate alert is visible, a suppressed one is silently absent
a_motif_hash_is_read_from_the_json_field_not_matched_as_a_substring ✅  extract_motif_hash reads the `motif_hash` field; the same hash sitting in `description` is not a prior alert (the `result_json CONTAINS` form this replaced could not tell the difference)
compute_motif_hash_is_hex_string                  ✅  output is non-empty, all chars ascii hex digits
compute_motif_hash_differs_for_different_inputs   ✅  different officer_id + case_number → different hashes
compute_motif_hash_is_pinned                      ✅  "director:john-smith"::"INS-2023-00123" → pinned hex value (regression fence)
```

**`crates/cli/src/commands/verify.rs`** — inline unit tests:

```
test_risk_level_display_low                       ✅  risk_banner(RiskLevel::Low) label contains "LOW"
test_risk_level_display_critical                  ✅  risk_banner(RiskLevel::Critical) label contains "CRITICAL"
test_format_verdict_text_all_fields               ✅  verdict text carries the entity name, "Risk Level", p_c to 2dp, every motif name, and the dissolved/officer counts
test_output_format_md_rejected                    ✅  --output md/pdf → Err mentioning "md/pdf" (use `report` for document output)
test_ch_number_validation_rejects_invalid         ✅  empty, too short ("a"), too long (11 chars), injection char ("abc; DROP"), hyphen ("abc-def") → false. Letters are NOT rejected — `is_valid_ch_number_cli` is `is_ascii_alphanumeric()` + length 2..=10, so `SC`/`NI`/`OC` prefixes pass, as they must. No space is tested; "abc; DROP" fails on `;`, not on its space.
test_ch_number_validation_accepts_valid           ✅  "12345678", "SC123456" → true
test_evasion_alert_company_number_discriminator   ✅  company A's evasion alert does not flag company B (cross-contamination discriminator)
```

#### Integration test files

**`crates/enricher/tests/psc_adapter.rs`** — 22 tests, fixture + offline (DB-gated for 8):

```
psc_parse_individual_fixture                      ✅  §1 fixture parses; company_number from resource_uri (not data field)
psc_parse_sc_prefixed_company_number              ✅  SC123456 extracted from resource_uri
psc_corporate_skipped                             ✅  corporate-entity kind → skip_if_corporate()=true
psc_individual_not_skipped                        ✅  individual kind → skip_if_corporate()=false
psc_exact_link_off_composite                      ✅  OFF composite match → ConfirmedLink at 1.0, auto_linked, evidence_json flat string
psc_exact_link_director_slug_fallback             ✅  director slug fallback → ConfirmedLink at 1.0, resolution_type="director_slug"
psc_no_match_standalone                           ✅  no officer match → no identity_link produced
adapter_disabled_when_no_key                      ✅  PscAdapter(None) → is_enabled()=false
adapter_disabled_when_empty_key                   ✅  PscAdapter(Some("")) → is_enabled()=false
adapter_disabled_when_whitespace_key              ✅  PscAdapter(Some("   ")) → is_enabled()=false
adapter_enabled_with_real_key                     ✅  PscAdapter(Some("key")) → is_enabled()=true
psc_company_number_from_resource_uri_not_data_field ✅  data.company_number absent; extracted from resource_uri (Pitfall 2 guard)
psc_controls_edge_has_temporal_meta               ✅  Controls edge serializes with effective_from (temporal invariant)
psc_adapter_run_disabled_returns_empty_stats      ✅  disabled adapter → no mutations emitted
test_match_officer_finds_director_by_hyphen_slug  ✅  DB-gated: match_officer returns director:{hyphen-slug}
test_match_officer_returns_none_when_not_found    ✅  DB-gated: unknown slug → None
test_process_psc_line_writes_identity_link_on_match ✅  DB-gated: matched officer → identity_link_written=true
test_process_psc_line_no_link_without_officer_match ✅  DB-gated: no officer node → identity_link_written=false
test_write_identity_link_propagates_db_error      ✅  DB-gated (regression): write_identity_link returns Err on unauthenticated DB — previously swallowed the error as Ok(()) causing silent data loss on session expiry
```

**`crates/enricher/tests/disqualified_adapter.rs`** — 22 tests, fixture + offline (DB-gated for 6):

```
disq_fixture_parses_forename_surname_dob          ✅  §1 fixture: forename="John", surname="Smith", DOB="1970-03" (month+year, Pitfall 3)
disq_fixture_year_only_dob_parses                 ✅  2nd line: DOB "1985" → ParsedDob{year:1985, month:None}
disq_fixture_no_dob_parses_as_none                ✅  3rd line: no date_of_birth → None
disq_confidence_full_month_year                   ✅  name + month + year → 0.95
disq_confidence_year_only                         ✅  name + year only → 0.90
disq_confidence_name_only_no_link                 ✅  name only (no DOB) → None (three-field minimum invariant)
disq_confidence_name_not_matched_is_none          ✅  name_matched=false → None regardless of DOB
disq_edge_meta_effective_from_matches_disqualified_from ✅  DisqualifiedFrom edge effective_from == disqualified_from date
disq_edge_meta_case_identifier_to_case_number     ✅  case_identifier field → case_number on DisqualifiedFromData
disq_link_full_dob_score_is_095                   ✅  full DOB → score=0.95, auto_linked, source="ch_disqualified"
disq_link_year_only_score_is_090                  ✅  year-only DOB → score=0.90
disq_link_evidence_json_is_flat_string            ✅  evidence_json is flat JSON string; fields name_slug, dob_year, dob_month, match
disq_adapter_disabled_when_no_key                 ✅  DisqualifiedAdapter(None) → is_enabled()=false
disq_adapter_disabled_when_empty_key              ✅  DisqualifiedAdapter(Some("")) → is_enabled()=false
disq_adapter_enabled_with_real_key                ✅  DisqualifiedAdapter(Some("key")) → is_enabled()=true
disq_adapter_name_is_ch_disqualified              ✅  adapter.name() == "ch_disqualified"
test_match_disqualified_officer_by_hyphen_slug    ✅  DB-gated: match by director:{hyphen-slug}
test_match_disqualified_officer_none_when_not_found ✅  DB-gated: unknown slug → None
test_process_disq_line_writes_identity_link_on_match ✅  DB-gated: officer found + DOB present → identity_link_written=true
test_process_disq_line_name_only_discarded        ✅  DB-gated: no DOB → identity_link_written=false
test_write_disq_identity_link_propagates_db_error ✅  DB-gated (regression): write_disq_identity_link returns Err on unauthenticated DB — mirrors the PSC fix; both functions previously swallowed errors causing silent data loss on session expiry
```

**`crates/enricher/tests/evasion_detector.rs`** — 6 tests, all DB-gated (require `URBAN_DB_TESTS=1`):

```
evasion_fires                    ✅  Active officer + auto_linked 0.95 + today-in-window → ≥1 MotifDetected row, motif_type="disqualified_director_evasion", risk_score=1.0
evasion_candidate_suppressed     ✅  Active officer + candidate 0.72 link + today-in-window → ZERO alerts (status filter)
evasion_window_guard             ✅  Case A: officer with past-window ban → ZERO alerts; Case B: dissolved officer + today-in-window → ZERO alerts
a_correction_dissolved_officer_does_not_alert ✅  a later correction to Dissolved suppresses the alert
a_prefix_id_does_not_borrow_another_officers_status ✅  the status lookup was a `contains` match, so `director:john` picked up `director:john-smith`'s row
```

**`crates/enricher/tests/engine_coverage.rs`** — 13 tests, all DB-gated (require `URBAN_DB_TESTS=1`):

```
test_engine_skips_adapter_out_of_jurisdiction ✅  an adapter whose coverage excludes the configured nation is not run
test_engine_runs_adapter_in_jurisdiction     ✅  the converse — an in-jurisdiction adapter is run
test_engine_run_source_refuses_out_of_jurisdiction ✅  run_source refuses rather than silently no-opping
test_engine_new_and_run_all_no_adapters      ✅  EnricherEngine::new() + run_all_enabled() with no adapters → zero stats
test_engine_run_all_enabled_skips_disabled   ✅  disabled adapter stats excluded; only enabled adapter contributes
test_engine_run_all_continues_on_adapter_error ✅  failing adapter swallowed (warn!); succeeding adapter stats preserved
test_engine_run_source_found_and_enabled     ✅  run_source("test_source") happy path → correct stats
test_engine_run_source_found_but_disabled    ✅  disabled adapter via run_source → empty stats returned
test_engine_run_source_not_found             ✅  unknown source name → warn! + empty stats, no panic
test_engine_start_background_fires_after_interval ✅  zero-interval background loop fires within 200ms
test_engine_reauth_refreshes_own_session     ✅  regression: engine.reauth() calls signin() on self.db directly — previously called on db.clone() (independent session), leaving the engine's own session expired
test_surreal_clone_invalidate_leaves_original_usable ✅  regression: Surreal::clone() creates an independent session; invalidating the clone leaves the original handle usable — documents the root cause of the Gazette 401 failures
```

**`crates/cli/tests/osint_surface.rs`** — no DB required:

```
osint_section_skeleton_empty_profile_renders_all_headings     ✅  (see OSINT Surface Tests section above)
osint_section_skeleton_empty_profile_all_none                 ✅
osint_section_skeleton_populated_beneficial_owner             ✅
verify_osint_flags_disqualified_director_flag                 ✅
verify_osint_flags_active_evasion_overrides_critical          ✅
verify_osint_flags_critical_maps_to_exit_1                    ✅
verify_osint_flags_phase17_stubs_are_false                    ✅
verify_osint_flags_beneficial_owners_populated                ✅
verify_osint_flags_no_evasion_no_override                     ✅
```

#### Running the OSINT Adapter Tests

```bash
# All adapter + evasion tests (no DB required for offline/fixture tests)
cargo test -p urban-integrity-enricher

# PSC adapter tests only
cargo test -p urban-integrity-enricher adapters::ch_psc

# Disqualified adapter tests only
cargo test -p urban-integrity-enricher adapters::ch_disqualified

# Evasion detector tests only
cargo test -p urban-integrity-enricher evasion

# verify command tests
cargo test -p urban-integrity-cli commands::verify

# OSINT surface tests
cargo test --test osint_surface -p urban-integrity-cli

# DB-gated enricher tests (PSC, disqualified, evasion, engine coverage)
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test psc_adapter
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test disqualified_adapter
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test evasion_detector
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test engine_coverage
```

---

### Growth: Property, Insolvency and Land Registry

The Land Registry CCOD adapter, the London Gazette insolvency adapter, the Property Accumulation Anomaly detector and the supporting CCOD-dedup logic landed together. Their tests span unit tests in new source files and dedicated integration test files.

#### `crates/enricher/tests/ccod_adapter.rs` — CCOD adapter tests

Unit tests drive the CSV-streaming logic with fixture ZIPs — no HTTP, no SurrealDB required. One integration test (idempotency via DB) is `URBAN_DB_TESTS=1`-gated.

```
test_ccod_adapter_emits_property_nodes         ✅  fixture with 1 filled slot → 1 CcodRecord; title_number, company_number, price_paid, proprietor_slot correct
test_ccod_two_slots_emits_two_records          ✅  fixture with slots 1+2 non-empty → 2 distinct records with correct company numbers
test_ccod_adapter_skips_empty_slots            ✅  slots 1+3 non-empty, 2+4 empty → 2 records; slot 2 and 4 absent
test_ccod_adapter_invalid_ch_number_skipped    ✅  "INVALID!" CH number → 0 records (validation gate)
test_ccod_streaming_bounded                    ✅  10 000-row fixture streams without OOM (streaming guarantee)
test_ccod_adapter_all_four_slots               ✅  all 4 slots non-empty → 4 records; slots 1–4 all present
test_ccod_adapter_disabled_when_no_key         ✅  api_key=None → is_enabled()=false; zero mutations sent
test_ccod_evidence_json_is_flat_string         ✅  evidence_json is flat JSON string with title_number, company_number, proprietor_slot, score fields
test_parse_ccod_date_valid                     ✅  "01-05-2024" (DD-MM-YYYY) → 2024-05-01 UTC midnight
test_parse_ccod_date_invalid                   ✅  "not-a-date" and "" → None
test_get_field_by_name_lookup                  ✅  positional field lookup by header name; missing header → None
test_extract_full_file_name_picks_latest_full  ✅  HMLR dataset listing → lexically-greatest CCOD_FULL_YYYY_MM.zip chosen
test_extract_full_file_name_absent             ✅  listing with no FULL file → None; auth-failure body (no `result`) → None, no panic
test_extract_download_url_signed               ✅  file response → signed S3 download_url extracted
test_extract_download_url_error_body           ✅  unsigned-licence error body → None
test_ccod_idempotent                           ✅  (DB-gated) running the same fixture twice → identity_link count before + 1 (UPSERT, not duplicate)
```

#### `crates/enricher/tests/gazette_adapter.rs` — London Gazette adapter tests

Unit tests use mock `serde_json::Value` responses — no live HTTP. One integration test (cursor persistence) is `URBAN_DB_TESTS=1`-gated.

**`gazette_fetch_json` retry behaviour:** `gazette_fetch_json` attempts up to 3 fetches with a 90-second back-off between retries. A non-JSON response (e.g. an HTML rate-limit page) triggers a retry. If all 3 attempts fail, the function returns `None` and the search page is skipped for that notice code. The 90s delay is chosen so that 3 attempts span ~270s, which exceeds the Gazette's ~5-minute IP-ban window.

```
gazette_adapter_is_always_enabled                    ✅  LondonGazetteAdapter::is_enabled() returns true unconditionally (public API, no credential required)
gazette_adapter_name_is_london_gazette               ✅  adapter.name() == "london_gazette"
gazette_parse_date_iso8601                           ✅  "2024-03-15T00:00:00Z" → Ok, formatted "2024-03-15"
gazette_parse_date_date_only                         ✅  "2024-03-15" (date-only) → Ok (Gazette published field uses both formats)
gazette_notice_codes_inliquidation_only              ✅  NOTICE_CODES contains 2452, 2454, 2410; does NOT contain 2460 or 2408 (ratified scope); len=3
gazette_extract_company_number_from_graph_array      ✅  companyNumber in @graph array found via find_map (A2); "12345678" returned
gazette_extract_company_number_missing_returns_none  ✅  no companyNumber in @graph → None (no fuzzy fallback)
gazette_extract_company_number_empty_graph_returns_none ✅  @graph: [] → None
gazette_extract_company_number_no_graph_field_returns_none ✅  companyNumber at JSON-LD top level NOT extracted (Pitfall 3 guard)
gazette_adapter_skips_notice_without_company_number  ✅  JSON-LD without companyNumber → zero UpdateNode mutations
gazette_adapter_emits_update_node                    ✅  JSON-LD with companyNumber + existing weight → exactly 1 UpdateNode; entity_id="GB-COH:12345678"; status=InLiquidation
gazette_adapter_cursor_advances                      ✅  (DB-gated) cursor timepoint advances after persist; load_timepoint() returns Some after persist; clears on clear_timepoint()
```

#### `crates/enricher/tests/property_accumulation.rs` — Property Accumulation Anomaly tests

All 5 tests are `URBAN_DB_TESTS=1`-gated. They use entity-specific prefixes and cleanup helpers so parallel runs stay idempotent.

The detector's dedup behaviour: before writing a `MotifDetected` event, the detector derives an idempotency key from `entity_id + "property_accumulation"` and checks whether a `MotifDetected` row with the same `motif_hash` already exists in `graph_events`. If it does, the write is skipped, preventing duplicate alerts on repeated runs.

```
test_property_accumulation_fires               ✅  Active entity + 4 OwnsProperty (£400k total) + 1 AwardedTo (£50k) → ratio 8× > 5× → exactly 1 MotifDetected; motif_type="property_accumulation_anomaly"; risk_score=0.75
test_property_accumulation_inactive_skip       ✅  InLiquidation entity + same edges → ZERO alerts (status gate)
test_property_accumulation_candidate_suppressed ✅  entity has edges but no NodeAdded event (only candidate identity_link at 0.72) → ZERO alerts (no NodeAdded → status unknown → NotActive)
test_property_accumulation_no_contracts        ✅  Active entity + 4 OwnsProperty but zero AwardedTo → ZERO alerts (≥1 contract required for ratio denominator)
a_prefix_id_does_not_borrow_a_longer_entitys_active_status ✅  the same substring-match defect on the entity status lookup — an exact `.get` miss must default to NotActive
```

#### Session-Expiry Regression Tests

Three regression tests were added on 2026-06-07 to guard against the class of silent data-loss bugs caused by SurrealDB session expiry during long-running enricher runs. All three are `URBAN_DB_TESTS=1`-gated.

**Root cause documented by `test_surreal_clone_invalidate_leaves_original_usable`:** `Surreal::clone()` creates an independent session (new UUID). Calling `signin()` on a clone does not refresh the original handle's session. The Gazette reauth task held a clone and kept refreshing it; the engine's original handle expired unnoticed, causing 401 errors on every subsequent write.

**Fix verified by `test_engine_reauth_refreshes_own_session`:** `engine.reauth()` now calls `signin()` directly on `self.db` rather than on a `db.clone()`.

**Write propagation verified by `test_write_identity_link_propagates_db_error` and `test_write_disq_identity_link_propagates_db_error`:** Both `write_identity_link` (PSC adapter) and `write_disq_identity_link` (Disqualified adapter) previously swallowed all DB errors as `Ok(())`. They now propagate errors so callers fail fast and visibly rather than silently dropping writes.

```
test_engine_reauth_refreshes_own_session         ✅  engine_coverage.rs — engine.reauth() succeeds with valid credentials; run_all_enabled() succeeds after reauth
test_surreal_clone_invalidate_leaves_original_usable ✅  engine_coverage.rs — invalidating session B (clone) does not affect session A (original); sessions are independent (SDK invariant documentation)
test_write_identity_link_propagates_db_error      ✅  psc_adapter.rs — write_identity_link on unauthenticated DB returns Err (not Ok)
test_write_disq_identity_link_propagates_db_error ✅  disqualified_adapter.rs — write_disq_identity_link on unauthenticated DB returns Err (not Ok)
```

**Drain-session follow-up (2026-07-26, commit 5a2d7bf):** the same clone-session trap resurfaced
in the CLI `enrich` command's mutation-drain task — `drain_to_wal` wrote through a
`Surreal::clone()` session that the hourly engine re-auth never touched, so any enrich run longer
than the token TTL lost every mutation drained after expiry (the 10-hour ICIJ run ended with all
end-of-run Electoral Commission `political_actor`/`donates_to` events dying on 401). The drain
session is now `Arc`-shared with the re-auth loop (`reauth_session` in
`crates/cli/src/commands/enrich.rs`) and re-signed on the same hourly cadence — and both re-auths
re-select the *resolved* analysis database, where previously an `--analysis` run silently switched
back to the config-default DB at the first refresh.

#### Running the Property and Insolvency Tests

```bash
# All property/insolvency enricher tests (CCOD + Gazette unit tests run without DB)
cargo test -p urban-integrity-enricher

# CCOD adapter tests only (unit tests — no DB)
cargo test -p urban-integrity-enricher --test ccod_adapter

# CCOD idempotency test (requires DB)
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test ccod_adapter test_ccod_idempotent

# Gazette adapter tests only (unit tests — no DB)
cargo test -p urban-integrity-enricher --test gazette_adapter

# Gazette cursor integration test (requires DB)
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test gazette_adapter gazette_adapter_cursor_advances

# Property accumulation tests (all require DB)
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test property_accumulation

# Property accumulation POSITIVE test only
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test property_accumulation test_property_accumulation_fires

# Session-expiry regression tests
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test engine_coverage test_engine_reauth_refreshes_own_session
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test engine_coverage test_surreal_clone_invalidate_leaves_original_usable
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test psc_adapter test_write_identity_link_propagates_db_error
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test disqualified_adapter test_write_disq_identity_link_propagates_db_error

# All property/insolvency DB-gated tests
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test ccod_adapter --test gazette_adapter --test property_accumulation --test engine_coverage --test psc_adapter --test disqualified_adapter
```

---

### Growth: Registry Adapters and Cross-Jurisdiction Detectors

The registry adapters (Charity Commission, Electoral Commission, ICIJ, OpenCorporates, OpenSanctions, council registers) and four cross-jurisdiction detector modules added a large body of inline tests. All of them run without external services or a DB.

> **Exact-match-only refactor (2026-06-13, commit 43675f8):** the in-app name-similarity
> machinery these adapters originally used — `resolver.rs` (`composite_score`,
> `surname_phonetic_codes`, `ascii_fold`) and its unit tests — was deleted. Project policy is
> now exact-identifier joins only (name-only fuzzy matching produced the ~87k false-positive
> charity links purged 2026-06-11 and 3,513 EC links disabled 2026-06-13). The
> `crates/enricher/src/resolver.rs` test block that previously appeared in this section no
> longer exists. The remaining enricher adapters (council registers, OpenCorporates, ICIJ,
> Electoral Commission) now perform exact joins; OpenSanctions delegates name matching to the
> external yente service, not the in-app resolver.

#### `crates/enricher/src/charity_crossover.rs` — Charity Crossover unit tests (no DB)

```
test_enr11_negative_candidate_produces_zero_candidates  ✅  candidate-status trustee row → zero CrossoverCandidates (gate)
test_enr11_positive_auto_linked_produces_one_candidate  ✅  auto_linked trustee row → one CrossoverCandidate
test_compute_motif_hash_stable                          ✅  deterministic hex string for same inputs
test_process_trustee_rows_empty_inputs                  ✅  empty inputs → empty Vec, no panic
test_officer_not_in_directs_set_is_skipped              ✅  officer not holding a contract → zero candidates (no false crossover)
test_confirmed_status_is_accepted                       ✅  confirmed status accepted alongside auto_linked
test_sub_candidate_status_is_rejected                   ✅  sub_candidate rows must not fire alerts
test_recordid_coercion_officer_id                       ✅  RecordId object source_node_id → partial match succeeds
test_null_source_node_id_is_skipped                     ✅  null source_node_id → skipped gracefully
test_multiple_rows_only_matching_returned               ✅  mixed rows → only officer-in-directs auto_linked/confirmed returned
```

#### `crates/enricher/src/offshore_shadow.rs` — Offshore Shadow unit tests (no DB)

```
test_enr11_negative_candidate_produces_zero_candidates  ✅  candidate-status ICIJ link row → zero MotifCandidates (gate)
test_enr11_positive_auto_linked_produces_one_candidate  ✅  auto_linked ICIJ link row → one MotifCandidate
test_offshore_shadow_ignores_065                        ✅  negative: a deliberately-bounded pending_review@0.65 OCOD link → zero candidates (the ≥0.85 gate is the defamation guard)
test_compute_motif_hash_stable                          ✅  deterministic hex string for same entity_id + motif_type
test_process_link_rows_empty_inputs                     ✅  empty inputs → empty Vec, no panic
test_entity_not_in_contract_set_is_skipped              ✅  entity not holding any contract → zero candidates (no false positives)
test_confirmed_status_is_accepted                       ✅  confirmed status also passes gate
test_sub_candidate_status_is_rejected                   ✅  sub_candidate status → rejected
test_recordid_coercion_source_node_id                   ✅  RecordId object source_node_id → partial match succeeds
test_null_source_node_id_is_skipped                     ✅  null source_node_id → skipped gracefully
test_multiple_rows_only_matching_returned               ✅  mixed rows → only entity-in-contracts auto_linked/confirmed returned
```

#### `crates/enricher/src/donation_correlation.rs` — Donation Correlation unit tests (no DB)

16 tests. Four of them arrived 2026-08-14 with PR #36, which changed `lookup_lpa_codes` from a
two-arm resolution (exact → normalised) to a three-arm one: **exact → declared alias → normalised**.

```
compute_motif_hash_is_deterministic                     ✅  same inputs → same hash on repeated calls
compute_motif_hash_is_hex_string                        ✅  non-empty, all chars ascii hex digits
compute_motif_hash_differs_for_different_inputs         ✅  different donor_id → different hash
compute_motif_hash_is_pinned                            ✅  SHA-256 digest pinned as regression fence
enr_11_status_filter_present_in_query                   ✅  source-level assertion: "status IN [$auto_linked, $confirmed]" clause present in query string
day_730_fires_day_731_does_not                          ✅  boundary: day 730 fires; day 731 does not; same-day fires
party_authority_map_loads_from_json                     ✅  the shipped data/party_authority_control.json deserialises to PartyAuthorityMap and contains "Labour Party" (uses a bare serde_json::from_str, so it covers arm 1 only)
party_name_normalisation_matches_variants               ✅  "The Labour Party" normalises to same key as "Labour Party"; case-insensitive
party_authority_map_lookup_normalised                   ✅  arm 3: normalised lookup "The Labour Party" succeeds against a "Labour Party" key
ec_registered_name_resolves_via_the_declared_alias_table ✅  arm 2: "Conservative and Unionist Party" reaches the "Conservative Party" key via _meta.name_variants — and asserts first that the two names differ under normalisation, so the test cannot pass by accident through arm 3
alias_lookup_is_case_insensitive_on_the_ec_name         ✅  "CONSERVATIVE AND UNIONIST PARTY" still resolves — a feed that changes only casing must not drop back to a miss
missing_or_malformed_name_variants_degrades_rather_than_failing ✅  an absent _meta.name_variants, and one that is an array rather than an object, both parse to an empty alias table with exact/normalised lookup still working — degrade, don't fail the load
every_party_name_seen_in_live_ec_data_resolves_against_the_shipped_map ✅  pins data/party_authority_control.json itself, not a fixture: all three party names observed in live identity_links.evidence_json resolve
```

**Why the alias arm was needed.** `data/party_authority_control.json` keys on the colloquial names
Open Council Data uses (`"Conservative Party"`), while the Electoral Commission publishes registered
*legal* names (`"Conservative and Unionist Party"`). The file's `_meta.name_variants` table had
always recorded that mapping and `PartyAuthorityMap` had always deserialised `_meta` into memory —
`lookup_lpa_codes` simply never consulted it. `normalise_party_name` cannot bridge the gap: it
lowercases and strips a leading `"the "`, while these two names differ by two whole words. Every
Conservative donation therefore returned `None` at the party gate — 13 of the 22 EC links in the
live database.

**Two traps this section exists to record.** First, `aliases` is `#[serde(skip)]`, so a bare
`serde_json::from_str` yields an empty alias table and silently restores the bug; construct through
`PartyAuthorityMap::from_json_str`, which is what the four new tests use. Second,
`every_party_name_seen_in_live_ec_data_resolves_against_the_shipped_map` deliberately reads the
shipped data file rather than a fixture — the bug was a mismatch between the real map's keys and the
names the EC actually publishes, and a hand-built fixture would have agreed with the code instead of
catching it.

These tests assert **resolution**, not that the detector fires. A third blocker is still open and
out of scope for them: `party_authority_control.json` holds 44 LPA codes, all English and in
perfectly sequential blocks, i.e. placeholder data. The maintainers' Welsh reference council appears
nowhere in it, so the LPA intersection cannot match whatever the donation dates say. Closing that needs real
council-control data covering Welsh unitary authorities — a data-sourcing decision, not a code
change.

#### `crates/enricher/src/ec_date.rs` — EC `AcceptedDate` parsing (6 tests, no DB)

Added 2026-08-14 with PR #35 as the single parser both Electoral Commission call sites now route
through. The EC donations CSV publishes `AcceptedDate` as **`DD/MM/YYYY`** (e.g. `20/12/2010`), not
ISO, and two sites had independently assumed ISO and parsed with `%Y-%m-%d`: the adapter discarded
the failure with `.ok()`, so the typed `accepted_date` was `None` for every EC record ever ingested
with no log line to show for it, and `donation_correlation` read the raw string back, applied
`%Y-%m-%d` again, and skipped the donation — so that detector had never fired on real data. The
quiet failure masked the loud one.

```
parses_the_day_first_format_the_ec_feed_actually_publishes ✅  five DD/MM/YYYY values taken verbatim from live identity_links.evidence_json parse and round-trip
day_first_maps_to_the_right_day_and_month               ✅  "01/04/2003" → 2003-04-01 — 1 April, not 4 January, the failure mode a permissive "try both" parser would invite (it does not pin the match order: `/` vs `-` makes the two formats unambiguous, so reordering `EC_DATE_FORMATS` leaves this green)
still_accepts_iso_for_rows_written_after_this_fix       ✅  "2023-10-15" still parses, so rows written either side of the fix are both readable
tolerates_surrounding_whitespace                        ✅  "  20/12/2010  " parses after the trim
rejects_junk_rather_than_guessing                       ✅  "", "not-a-date", "2010", "20-12-2010", "12/2010" → None; callers are expected to log and skip, not silently drop
to_iso_normalises_day_first_and_passes_iso_through      ✅  to_iso("20/12/2010") = "2010-12-20"; ISO passes through unchanged; junk → None
```

Accepting **both** formats is deliberate rather than tidy-mindedness: rows written before the fix
hold `DD/MM/YYYY` in `evidence_json` while rows written after hold ISO, so an ISO-only reader would
make a full EC re-ingest a prerequisite for the fix.

**Why the suite stayed green through the original bug:** every pre-existing EC fixture used
`"2023-10-15"` — ISO, a format the live feed never emits. The tests encoded the same wrong
assumption as the code and so agreed with it perfectly. That is the standing lesson for fixture
choice here: a fixture invented alongside the parser proves only that the two agree, not that either
matches the feed. The tests above use values copied out of the live database instead.

#### New adapter inline tests (no DB)

**`crates/enricher/src/adapters/charity_commission.rs`** — 8 tests:

```
test_disabled_adapter_is_not_enabled                    ✅  api_key=None → is_enabled()=false
test_exact_company_number_produces_auto_linked_record   ✅  CH number field match → ConfirmedLink at 1.0
test_header_position_fallback_registered_company_number ✅  RegisteredCompanyNumber header variant accepted
test_detect_company_number_field_last_resort_fallback   ✅  last-resort column detection path
test_detect_company_number_field_none_when_absent       ✅  no company_number column → None
test_get_field_by_name_missing_returns_none             ✅  missing header → None
test_get_field_by_name_trims_header_whitespace          ✅  header with leading/trailing spaces trimmed before comparison
test_parse_charity_zip_parses_mock_archive              ✅  in-memory ZIP fixture → parsed rows
```

**`crates/enricher/src/adapters/electoral_commission.rs`** — 13 tests:

The three `accepted_date` tests arrived 2026-08-14 with PR #35. Alongside them the adapter changed
behaviour twice: it now normalises `AcceptedDate` to ISO when writing `evidence_json` (falling back
to the raw feed string when unparseable, so the audit trail never loses what the EC actually
published), and it `warn!`s with the `ECRef` instead of dropping an unparseable date in silence.

```
test_is_enabled_returns_true_when_enabled               ✅  enabled=true → is_enabled()=true
test_is_enabled_returns_false_when_disabled             ✅  enabled=false → is_enabled()=false
test_process_company_row_valid_company_number_produces_identity_link_fields ✅  valid CH number row → identity_link fields correct
test_process_company_row_parses_the_day_first_date_the_ec_feed_publishes ✅  AcceptedDate "20/12/2010" → typed accepted_date 2010-12-20; a None here is the original bug
test_evidence_json_accepted_date_is_normalised_to_iso   ✅  AcceptedDate "01/04/2003" → evidence_json.accepted_date == "2003-04-01", so downstream readers get one format
test_evidence_json_accepted_date_round_trips_through_the_shared_parser ✅  cross-site contract over "20/12/2010", "01/04/2003" and "2023-10-15": whatever the adapter stores must parse via ec_date::parse_ec_date — this is the untested seam that let the adapter and the detector drift apart
test_process_company_row_empty_company_number_returns_none ✅  empty company_number → None
test_classify_individual_confidence_routing             ✅  individual donor routing through confidence tiers
test_company_row_evidence_json_is_flat_string_not_nested ✅  evidence_json is flat string, not nested object
test_process_company_row_clean_number                   ✅  clean 8-digit number → is_prefix_ambiguous=false, original preserved
test_process_company_row_ambiguous_six_digit            ✅  6-digit number → Some with is_prefix_ambiguous=true
test_process_company_row_trailing_question              ✅  "12345678?" → trailing '?' stripped, number accepted
test_process_company_row_invalid                        ✅  "INVALID" → None after the normalise_ch_number check
```

**`crates/enricher/src/adapters/icij.rs`** — 11 tests:

> **Silent-write-failure rewrite (2026-07-26, commit 33e8850):** every ICIJ insert
> (814k entities + 3.3M links) had failed silently for the table's entire life — the adapter
> omitted the required `dataset` field and wrote `node_id_start`/`node_id_end` where `SCHEMA_DEF`
> defines `from_node_id`/`to_node_id`, and a `let _ =` discarded every write error while the
> counters kept counting attempts. The rewrite derives insert rows from `SCHEMA_DEF` field names,
> replaces per-row round-trips (4.1M queries, ~10h) with batched `INSERT IGNORE` (idempotent via
> the `UNIQUE` node_id index + the count guard), and propagates write errors instead of
> swallowing them. The former `test_icij_entity_upsert_query_uses_bound_parameters` test was
> removed with the per-row upsert; the schema-derived regression tests below make any future
> schema/adapter field drift fail offline. Never discard a SurrealDB write `Result`.

```
test_idempotency_check_skips_when_count_nonzero         ✅  existing icij node count > 0 → skip re-import (query string)
test_csv_row_validation_empty_fields_are_skipped        ✅  blank name/jurisdiction fields → row skipped
test_parse_icij_zip_extracts_entity_record              ✅  in-memory ZIP with entity CSV → parsed IcijEntity
test_parse_icij_zip_accepts_source_id_column_variant    ✅  "source_id" column variant accepted alongside "node_id"
test_entity_row_satisfies_schema_required_fields        ✅  entity insert row carries every required string field parsed out of SCHEMA_DEF; None optionals omitted (explicit null fails option<string> coercion)
test_link_row_satisfies_schema_required_fields          ✅  link insert row carries every SCHEMA_DEF-required field (from_node_id/to_node_id/dataset)
test_relationships_parse_carries_source_id_with_fallback ✅  relationships.csv sourceID → dataset; missing column falls back to "unknown"
get_field_by_name_returns_matching_column               ✅  named column resolved to its positional value
get_field_by_name_trims_header_whitespace               ✅  header with surrounding whitespace still matches
get_field_by_name_unknown_column_is_none                ✅  unknown column name → None
get_field_by_name_short_record_is_none                  ✅  record shorter than the header index → None
```

**`crates/enricher/src/adapters/opencorporates.rs`** — 19 tests. The eight gating tests below,
plus 11 covering the quota window and the alert-target reader (`alerted_ids_*`,
`test_quota_window_*`, `non_ascii_punctuation_is_encoded_as_utf8_bytes`,
`a_donation_correlation_alert_names_its_donor_as_a_target`,
`entity_id_and_participants_are_still_read`) — the latter group exists because the target reader
silently missed every participant of a core alert while reporting success:

```
test_is_enabled_false_when_licence_not_confirmed        ✅  licence not confirmed → is_enabled()=false
test_is_enabled_false_when_api_key_none                 ✅  api_key=None → is_enabled()=false
test_quota_exhausted_returns_ok_default                 ✅  quota-exhausted response → Ok(default stats), no panic
test_rate_limiter_constructs_without_panic              ✅  RateLimiter::new() completes without panic
test_licence_gate_takes_precedence                      ✅  licence gate fires before api_key check
test_is_enabled_true_when_all_gates_pass                ✅  api_key present + licence confirmed → is_enabled()=true
test_licence_gate_fail_hard                             ✅  enabled + licence unconfirmed → Err naming `oc_open_data_confirmed = true`
test_disabled_silent_skip                               ✅  enabled=false → Ok (silent skip, no error)
```

**`crates/enricher/src/adapters/opensanctions.rs`** — 6 tests:

```
test_d06_fail_hard_enabled_not_confirmed                ✅  enabled but not confirmed → fail-hard error (not silent skip)
test_d06_silent_skip_disabled                           ✅  disabled adapter → silent skip, empty stats
test_d06_happy_path_is_enabled                          ✅  enabled + confirmed → is_enabled()=true
test_d08_name_only_cap                                  ✅  name-only yente score (0.92) capped at 0.65 (below AUTO_LINK_THRESHOLD); cap lifted when DOB present
test_batch_serialisation                                ✅  batch request serializes to expected JSON shape
test_status_routing                                     ✅  score bands route to correct LinkStatus variants
```

**`crates/enricher/src/adapters/council_registers.rs`** — 9 tests (2 of them DB-gated:
`entity_name_index_reads_companies_not_officers` and `entity_name_index_pages_past_the_page_size`,
which pin the entity-name index this adapter resolves declarations against):

```
test_is_enabled_false_when_disabled                     ✅  disabled=true → is_enabled()=false
test_is_enabled_false_when_lpa_codes_empty              ✅  empty lpa_codes vec → is_enabled()=false
test_is_enabled_true_when_enabled_and_lpa_codes_set     ✅  enabled + codes present → is_enabled()=true
test_sidecar_unreachable_returns_ok_default             ✅  unreachable sidecar → Ok(default stats), no panic
test_parse_declaration_row_extracts_fields             ✅  declaration row → fields correctly extracted
test_extract_uids_from_response                         ✅  mock sidecar response → UIDs extracted correctly
test_sidecar_non2xx_gate_path                           ✅  scrape URL construction for the graceful-degradation path (localhost:8765/scrape)
```

#### Running the Registry Adapter Tests

```bash
# All registry-adapter and detector inline tests (no DB, no network)
cargo test -p urban-integrity-enricher

# Charity crossover unit tests only
cargo test -p urban-integrity-enricher charity_crossover

# Offshore shadow unit tests only
cargo test -p urban-integrity-enricher offshore_shadow

# Donation correlation unit tests only (16 tests, including the 4 party-alias regressions)
cargo test -p urban-integrity-enricher donation_correlation

# Shared EC AcceptedDate parser only (6 tests)
cargo test -p urban-integrity-enricher ec_date

# Individual adapter inline tests
cargo test -p urban-integrity-enricher adapters::charity_commission
cargo test -p urban-integrity-enricher adapters::electoral_commission
cargo test -p urban-integrity-enricher adapters::icij
cargo test -p urban-integrity-enricher adapters::opencorporates
cargo test -p urban-integrity-enricher adapters::opensanctions
cargo test -p urban-integrity-enricher adapters::council_registers
```

---

### Status-Transition Regression Tests

Two integration test files guard the `UpsertNode` status-transition fix (commit 5fdcf98 — before
it, a company observed while `Active` that later dissolved never had its status updated, so the
Phoenix detector could never fire for the normal monitoring case) and its replay half (commit
79f329e — paginated `graph_events` replay).

**`crates/core/tests/upsert_status_transition.rs`** — 1 end-to-end test, pure in-memory, no DB:

```
test_phoenix_fires_only_after_dissolution_upsert   ✅  ingest predecessor Active → re-ingest Dissolved via UpsertNode → active successor with shared officer + dated contract → PhoenixLoop fires; fails without the fix (predecessor would still read Active)
```

**`crates/core/tests/replay_status_transition.rs`** — 2 tests, `URBAN_DB_TESTS=1`-gated:

```
test_replay_folds_nodeupdated_over_nodeadded       ✅  replay_from_surreal folds a persisted NodeUpdated (active→dissolved) over the earlier NodeAdded by highest timestamp — restart reconstructs Dissolved, not the birth weight
test_paginated_replay_loads_all_rows_across_pages  ✅  fetch_events_paged loads every graph_events row across page boundaries — a populated graph never starts empty after restart
```

```bash
# In-memory upsert end-to-end test (no DB)
cargo test --test upsert_status_transition -p urban-integrity-core

# Replay regression tests (require DB)
URBAN_DB_TESTS=1 cargo test --test replay_status_transition -p urban-integrity-core
```

---

### Analysis Lifecycle Tests (`analysis create` / `list` / `remove`)

PRs #16 (analysis-lifecycle foundation) and #17 (analysis remove) added the CLI analyst-platform
analysis lifecycle: a `_catalog` registry table (`CATALOG_SCHEMA` in `crates/schema/src/db.rs`),
content-addressable analysis DB names (`an_<slug>_<wardset-hash>`), a global `--analysis` selector
resolved in `crates/cli/src/db.rs`, baseline cloning via `surreal export | surreal import`, and a
retention-guarded `remove` command.

#### `crates/cli/tests/analysis_lifecycle.rs` — 17 tests, all `URBAN_DB_TESTS=1`-gated

The two clone tests additionally require the `surreal` binary (resolved via `$SURREAL_BIN`, then
`~/.surrealdb/surreal`, then `$PATH`; CI has it via `surrealdb/setup-surreal`) and skip cleanly
without it.

```
registry_insert_get_list_roundtrip                 ✅  _catalog registry insert → get → list round-trip
registry_insert_duplicate_db_name_fails            ✅  duplicate db_name rejected (UNIQUE index)
resolve_main_ignores_registry                      ✅  no analysis selected → config-default DB, registry not consulted
resolve_unknown_analysis_errors                    ✅  unknown analysis name → error, not silent fallback
resolve_known_analysis_returns_db_name             ✅  registered analysis resolves to its an_<slug>_<hash> db name
clone_baseline_copies_graph_events                 ✅  (needs `surreal` binary) export|import clone copies graph_events rows into the new DB
create_empty_analysis_provisions_and_lists         ✅  `analysis create` provisions the DB + registry row; visible in `analysis list`
create_rejects_malformed_ward_code                 ✅  non-GSS ward code in the wards file → error
create_from_missing_baseline_errors                ✅  --from nonexistent baseline → error
create_from_baseline_clones_and_selection_isolates ✅  (needs `surreal` binary) --from clone; writes to the analysis DB do not leak into the baseline
registry_mark_retired_sets_status_and_timestamp    ✅  mark_retired → status="retired" + retired_at set
remove_dry_run_does_not_drop                       ✅  default dry-run reports the plan; DB still exists, registry row still active (self-certifying)
remove_apply_out_of_window_drops_and_retires       ✅  --apply outside the retention window drops the DB and retires the registry row
remove_reserved_names_hard_refused                 ✅  reserved database names are never removable, even with flags
remove_in_window_refused_without_decommission      ✅  inside the retention window → refused; DB intact, status still active
remove_in_window_with_decommission_drops           ✅  --decommission overrides the window guard → dropped + retired
remove_unknown_analysis_errors                     ✅  unknown analysis → error
```

#### Inline unit tests (no DB)

**`crates/core/src/analysis/naming.rs`** — 11 tests (pure naming module):

```
slugify_lowercases_and_replaces_non_alnum          ✅  label → lowercase slug, non-alphanumerics replaced
slugify_rejects_empty                              ✅  empty label → Err
wardset_hash_is_order_independent                  ✅  same ward set in any order → same hash
wardset_hash_is_dedup_invariant                    ✅  duplicate ward codes do not change the hash
wardset_hash_rejects_empty_set                     ✅  empty ward set → Err
validate_ward_code_accepts_ons_gss                 ✅  ONS GSS codes (e.g. E05...) accepted
validate_ward_code_rejects_malformed               ✅  malformed codes rejected
analysis_db_name_matches_identifier_regex          ✅  an_<slug>_<hash> is a valid SurrealDB identifier
analysis_db_name_is_idempotent                     ✅  same label + ward set → same db name
reserved_names_rejected                            ✅  reserved names refused as analysis db names
checkpoint_path_is_per_db                          ✅  checkpoint path is derived per analysis DB
```

**`crates/cli/src/commands/analysis.rs`** — 5 tests for the pure `within_window` retention helper.
The retention window falls back to 180 days when `detector.phoenix_window_days` is unset —
matching `DetectorConfigRaw`'s own default (fixed in commit 2ac028a; the fallback was previously
730):

```
within_window_true_for_recent                      ✅  recent created_at (180-day window) → true
within_window_false_for_old                        ✅  old created_at → false
within_window_boundary_just_inside                 ✅  just inside the boundary → true
within_window_boundary_just_outside                ✅  just outside the boundary → false
within_window_rejects_malformed                    ✅  malformed created_at timestamp → Err
```

**`crates/core/src/config.rs`** — 2 new tests for the `URBAN__ANALYSIS` selection field:

```
test_appconfig_analysis_defaults_to_none           ✅  AppConfig.analysis defaults to None
test_appconfig_analysis_deserializes_when_set      ✅  analysis field deserialises when set
```

#### Running Analysis Lifecycle Tests

```bash
# Full lifecycle suite (DB-gated; clone tests skip without the `surreal` binary)
URBAN_DB_TESTS=1 cargo test --test analysis_lifecycle -p urban-integrity-cli

# Point at a specific surreal binary for the clone tests
URBAN_DB_TESTS=1 SURREAL_BIN=$HOME/.surrealdb/surreal cargo test --test analysis_lifecycle -p urban-integrity-cli

# Pure naming-module unit tests (no DB)
cargo test -p urban-integrity-core analysis::naming

# Retention-window helper unit tests (no DB)
cargo test -p urban-integrity-cli within_window
```

---

### `donates_to` Projection Regression (debug: donates-to-projection-skip)

**`crates/ingestor/tests/projector_donates_to.rs`** — 1 `URBAN_DB_TESTS=1`-gated test (covered in the [SurrealDB Integration Tests](#surrealdb-integration-tests) section above).

This regression test drives the corrected Electoral Commission `DonatesTo` event shape through the real `Projector` against a live SurrealDB. It guards the data-model defect where the EC adapter emitted edges to `party:<slug>` recipients with no backing `political_actor` node and a record-id-form src — the combination that left the `donates_to` typed table empty after `project`. The corrected shape uses a bare `entity_id` src and a `PA:<slug>` `PoliticalActor` recipient; the test asserts exactly one clean `donates_to` relation row materialises (single `political_actor:` prefix, no doubled prefix) and that the recipient display name resolves via the read-path prefix strip.

```bash
# donates_to projection regression test (requires DB)
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_donates_to
```


---

## Persistence-Layer Guards

Three test surfaces exist because the failures they catch are **silent**: the query returns `Ok`,
the log reports success, and the row count is quietly wrong. No assertion at the call site can catch
that, so each guard constrains the *shape* instead.

### Row-losing query shapes (`crates/core/tests/query_shape_guard.rs` — 30 tests, no DB)

A source-level test that scans every non-test `.rs` file in the workspace for SQL string literals
and fails the build on two shapes that under-return without erroring:

1. **Indexed equality paired with an unindexed predicate.** On `graph_events`, `kind` is indexed
   (`idx_graph_events_kind`); pairing that equality with a `CONTAINS` on an unindexed `payload.*`
   sends the planner down the index path and returns a truncated, run-to-run non-deterministic row
   set. `count()` on the identical predicate stays correct, which is what makes it so hard to spot.
2. **A bare `LIMIT` with no paging.** A single large `LIMIT` returns well below the requested count
   when the rows are large, so the query samples a fraction of the table while looking like it read
   everything. An `ORDER BY … LIMIT` is flagged regardless of magnitude, because ordering plus a
   ceiling is a completeness read wearing a sample's clothes.

The project has paid for this three times: the CH backfill ran against 18% of its input set (6,033
rows where `count()` said 33,046); `ch_psc` enriched 0.27% of its candidate companies (39 rows where
`count()` said 14,198) while recording `candidates_found: 0` and reporting success; and the
`officer` / `psc` tables sat empty with no error anywhere in the logs.

**The guard is no longer single-table.** `WATCHED_TABLES` drives both the literal extractor and the
rule matcher from one list, so a table can only be added in one place. It currently covers **seven**
tables, each with its own Rule-2 threshold and its own indexed-field list for Rule 1:

| Table | `LIMIT` threshold | Indexed fields checked | Why the threshold is what it is |
|-------|------------------:|------------------------|---------------------------------|
| `graph_events` | 1,000 | `kind` | sized for `weight_json` response-size risk — a single large response can truncate well under a full-table read |
| `identity_links` | 1 | `source_node_id`, `target_node_id`, `status` | any unpaged sample here is suspect |
| `enrichment_runs` | 1 | *(none)* | the table is `SCHEMALESS` with no `DEFINE INDEX`, so Rule 1 has no index path to misuse |
| `oc_quota` | 1 | `year_month` | no production bare-`LIMIT` read exists today, so the low bar costs nothing |
| `land_flip_candidates` | 1 | `status`, `ppd_transaction_id`, `planning_ref` | `flips.rs` is recorded as a violation needing a fix, not a threshold dodge |
| `ppd_transactions` | 1,000 | `transaction_id`, `district_code`, `postcode_norm` | deliberately high: `land_flip.rs`'s legitimate `LIMIT 200` gate-1 join is tracked separately |
| `analyses` | 2 | `db_name` | deliberately not 1: `registry.rs`'s `WHERE db_name = $x LIMIT 1` is a genuine unique-key point lookup (`db_name` carries a UNIQUE index) |

Do not "fix" the two high thresholds back down to 1 without reading the comments above them in the
source — both were chosen to avoid flagging a genuinely safe read.

**`ALLOWLIST` is not empty, and its emptiness is not the goal.** It holds **6** entries, each
carrying a mandatory justification string so that "not fixed" can never be indistinguishable from
"not noticed":

| Exempt site | Why |
|-------------|-----|
| `crates/core/tests/surreal_integration.rs` | `test_query_entity_by_ch_number_regression_planner_bug` issues the buggy shape *and* the fixed shape against the same data and asserts they disagree — the disagreement is the bug signature |
| `crates/core/src/persistence/scan.rs` (`SELECT id FROM identity_links … LIMIT 1`) | `identity_link_exists` returns a bool; one row fully answers "does at least one exist", so discarding the rest is the semantics, not row-loss |
| `crates/core/src/persistence/scan.rs` (`identity_links … ORDER BY confidence DESC`) | deliberate pre-fix baseline: the A/B test proves the paged read reaches rows the old ceiling lost |
| `crates/core/src/persistence/scan.rs` (`land_flip_candidates … ORDER BY created_at DESC`) | same A/B shape, 150 seeded rows against a 40-row ceiling |
| `crates/cli/src/commands/links.rs` | pre-fix `LIMIT 100` shape kept verbatim so `fetch_pending_review_returns_every_row_past_the_old_confidence_ceiling` can prove the fix changes real behaviour |
| `crates/cli/src/commands/flips.rs` | same, on `land_flip_candidates` and `created_at` |

Two separate tests police the list, and they are not redundant:

- `every_allowlist_entry_is_justified_and_live` rejects an empty reason string and a fragment that
  no longer matches any file — a stale exemption fails loudly instead of quietly widening the hole.
- `allowlist_gained_no_entries_this_phase` asserts `ALLOWLIST.len() == ALLOWLIST_BASELINE` (**8**
  at `d828be7f`). This is the falsifiable form of a "zero new ALLOWLIST entries" bar — the first
  test passes happily against a newly added, well-justified entry, so it cannot enforce a count.
  Raising the baseline is a deliberate, reviewed edit; a dummy extra entry added in development
  fails it.

Two scope tests keep the scanner honest about *what* it reads: `test_paths_are_in_scope` (the walk
includes `crates/*/tests/`, not just `src/`) and `probe_subcommand_source_is_in_scope` (the whole
`crates/probe/src/` tree is scanned — the guard has to see the newest code, not just
the code that existed when it was written).

The remaining tests are self-tests of the matcher: it detects both live shapes and every per-table
fixture, does not flag `count()`, the paged replacements, a small `LIMIT` without `CONTAINS`, or the
unique-key `analyses` lookup, and is not blinded by char literals, escaped quotes, raw strings,
continuation escapes, or SQL quoted inside comments.

```bash
cargo test -p urban-integrity-core --test query_shape_guard
```

### Paged scanning (`crates/core/src/persistence/scan.rs` — 16 tests, 8 DB-gated)

The sanctioned replacement for the shapes above: keeps `kind` out of the `WHERE` clause, filters in
Rust, and pages by rows *actually received*.

```
a_plain_string_entity_id_passes_through          ✅  bare string entity_id read back unchanged
a_coerced_record_id_is_reassembled               ✅  RecordId-coerced value reassembled to its table:key string
a_record_id_with_a_bare_string_id_is_reassembled ✅  RecordId with a plain string id also reassembled
an_empty_or_missing_entity_id_is_none            ✅  empty / absent entity_id → None, not a panic
db_scan_returns_every_row_where_the_indexed_shape_truncates ✅  (DB) paged scan returns rows the indexed+CONTAINS shape loses
db_scan_by_kind_pages_past_a_hard_coded_ceiling  ✅  (DB) scan continues past any single-request ceiling
db_existence_check_finds_a_match_beyond_the_first_page ✅  (DB) existence check is not limited to page 1
db_any_type_scan_covers_every_marker_without_duplicating_rows ✅  (DB) multi-kind scan is complete and duplicate-free
db_identity_links_scan_pages_past_a_hard_coded_ceiling ✅  (DB) A/B against the old ORDER BY confidence DESC LIMIT 40 shape: the old query returns exactly 40 and omits the seeded marker row, the paged scan returns all of them
db_identity_links_scan_filters_by_status_and_source ✅  (DB) status/source filtering survives paging
db_identity_link_exists_finds_a_match_beyond_the_first_page ✅  (DB) the bool existence check is not page-1-limited
db_land_flip_candidates_scan_pages_past_a_hard_coded_ceiling ✅  (DB) same A/B, 150 seeded rows against the old 40-row ceiling
```

The four `identity_links` / `land_flip_candidates` tests cover the paged reads; the old shapes
they compare against are the reason several of the `ALLOWLIST` entries exist. The
matching CLI read paths have their own regressions — `fetch_pending_review_returns_every_row_past_
the_old_confidence_ceiling` in `crates/cli/src/commands/links.rs` and
`fetch_pending_review_returns_every_row_past_the_old_created_at_ceiling` in
`crates/cli/src/commands/flips.rs`, both DB-gated.

The ingestor has a re-authenticating equivalent, `backfill::scan_events_by_type`, covered by 3
DB-gated tests in `crates/ingestor/src/backfill/mod.rs`
(`db_scan_events_by_type_paginates_and_filters_kind`,
`db_scan_agrees_with_count_where_the_old_shape_did_not`,
`db_scan_returns_ids_in_rust_string_order`).

### Fail-loud WAL writes (`crates/enricher/src/wal_writer.rs` — 5 tests, 2 DB-gated)

```
test_build_wal_record_id_sanitises_special_chars ✅  special characters in id components sanitised
test_build_wal_record_id_deterministic           ✅  same components → same record id
test_build_wal_record_id_empty_components        ✅  empty components handled without panic
db_write_wal_event_errors_when_the_row_is_rejected ✅  (DB) a rejected row returns Err — never a silent Ok
db_write_wal_event_succeeds_and_stays_idempotent   ✅  (DB) repeat write does not duplicate the row
```

The second DB test is the direct descendant of the ICIJ silent-write failure: a discarded write
`Result` let an adapter report 814k rows written into an empty table. Never discard a SurrealDB
write `Result`.

---

## Projection Layer (`crates/ingestor/src/projector.rs` — 53 inline tests, no DB)

The projector turns `graph_events` rows into the typed tables the read path queries. Beyond the
node/edge table-mapping tests listed earlier, it now covers three failure classes that were all
silent in production:

**Event kinds.** `projector_reads_node_updated_events_too` — a projection pass that only read
`NodeAdded` left every status transition (active → dissolved) unprojected.

**Schema-vs-projector field drift.** `every_schema_defined_contract_field_is_projected` and
`contract_projection_carries_received_date_provenance_and_handle` — a field defined in `SCHEMA_DEF`
but absent from the projector's JSON literal vanishes with no error at all. 30,836 events carried
`application_received_date` while the projected `contract` table held zero. The first test makes
that drift a compile-time-adjacent failure rather than a data-quality mystery.

**Batch attribution and identifier safety.**

```
one_rejected_statement_does_not_discard_its_batch     ✅  a single rejected statement no longer voids its whole batch
an_entirely_rejected_batch_is_attributed_statement_by_statement ✅  each rejection attributed individually
a_backslash_in_a_slug_cannot_break_the_statement      ✅  backslash in a slug cannot escape the statement
a_backtick_in_a_slug_cannot_terminate_the_identifier  ✅  backtick in a slug cannot terminate the identifier
an_ordinary_slug_is_untouched                         ✅  the escaping is a no-op on ordinary slugs
a_clean_batch_costs_exactly_one_request               ✅  no per-row round-trips on the happy path
an_unreachable_server_is_undelivered_not_rejected     ✅  transport failure classified as undelivered, not rejected
an_empty_batch_is_a_no_op                             ✅  empty batch issues no request
absorb_keeps_the_total_and_its_breakdown_in_step      ✅  stats total always equals the sum of its parts
skipped_attributed_covers_every_skip_reason           ✅  every skip reason is attributed, none silently dropped
the_live_regression_shape_would_now_be_visible        ✅  the shape of the live regression now surfaces in the stats
test_edge_skip_ratio_basic                            ✅  skip ratio computed over edges
test_skip_ratio_ok_for_no_edges_and_clean_runs        ✅  zero edges and clean runs pass the ratio check
test_skip_ratio_ok_passes_historical_debris           ✅  known historical debris does not fail the run
test_skip_ratio_fails_on_catastrophic_loss            ✅  catastrophic edge loss fails loudly
```

**Record-id construction.** The projector builds record ids at two independent sites (`upsert_node`'s
`$id` bind and `quote_record_id`), and both used to prepend the table name unconditionally — so an
`entity_id` that already carried its own `property:` prefix came out doubled as
``property:`property:xxx` ``. Measured live 2026-08-21: 105,809 of 105,809 `property` rows were
affected, while `offshore_entity` was clean (0 of 29,944). `strip_own_table_prefix` is applied at
both write sites:

```
property_double_prefix_is_stripped                    ✅  a `property:`-prefixed entity_id is not prefixed twice
strip_own_table_prefix_is_table_equality_only         ✅  only the row's OWN table name is stripped, never a lookalike
```

`crates/ingestor/tests/projector_property_prefix.rs::test_project_property_node_is_not_double_prefixed`
(1 test, DB-gated) is the other half: it drives one `property:` `NodeAdded` event through the real
`Projector` and asserts the record id SurrealDB actually stores is single-prefixed.

```bash
cargo test -p urban-integrity-ingestor projector
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_property_prefix
```

---

## OCOD Owner Merge and the Purge Path

Two CLI subcommands delete rows — `purge-orphans` and `purge-ocod-owners`, both in
`crates/cli/src/commands/purge.rs` — and a deletion that removes the wrong rows, or silently
removes none, is the failure this project has been burnt by most. The suite is therefore split
between pure tests of each control and DB-gated tests that prove the control works against a real
SurrealDB rather than against an in-test model of one.

### `crates/cli/src/commands/purge.rs` — 22 tests, 4 DB-gated

**Confirmation (pure).** The prompt takes a typed digest derived from *this run's* scan output —
the first 6 bytes of a SHA-256 over the sorted record ids — so it cannot be pre-baked into a
script, an alias, or shell history. There is deliberately no `--yes`, no `--force` and no
environment escape.

```
confirmation_digest_is_deterministic_and_order_independent ✅  same id set → same digest, whatever the order
digest_matches_trims_whitespace_but_never_case_folds       ✅  surrounding whitespace forgiven, case never is
confirm_interactively_returns_err_not_ok_false_when_stdin_is_not_a_terminal ✅ non-TTY is an error, not a silent "no"
```

The refusal message is asserted to name `--yes`, `--force` and "env escape" explicitly, so the
absence of an escape hatch is documented at the point of refusal. `purge_refuses_non_tty` in
`crates/cli/tests/cli.rs` is the DB-gated end-to-end counterpart: it spawns the real binary with
stdin closed against an isolated `TestNs` and asserts both a non-zero exit and that the seeded
orphan row is still present afterwards.

**Record-id handling (pure).** SurrealDB renders any record id whose key is not a bare identifier
in backtick-quoted form, and live `graph_events` keys are exactly that shape. Leaving the backticks
on makes `RecordId::new(table, key)` address a key whose literal characters *include* the
backticks, which matches nothing — so the delete removes 0 rows and looks like a no-op.

```
record_key_strips_table_prefix                        ✅  "table:key" → "key"
record_key_strips_the_backticks_surrealdb_quotes_keys_with ✅ a backtick-quoted key is unquoted before use; colons INSIDE a quoted key survive
```

**Orphan definition (pure).** The definition is ownership-edge based, not derived from the
`GB-COH:` company-number shape. That distinction is the whole point: an overseas-owned OCOD
property has a slugified owner segment that is not a GB company, but it carries an `OwnsProperty`
edge, and the older company-number-derived definition misclassified 93,052 such records as orphans.

```
property_with_owner_edge_is_not_orphan                ✅
property_without_owner_edge_is_orphan                 ✅
ocod_overseas_owned_property_is_kept_even_though_owner_is_not_gb_coh ✅ the 93k-record regression, pinned
```

**Removed-count and blast radius (pure).** `check_delete_count` compares keys *sent* against rows
SurrealDB reports *removed* and hard-errors on a mismatch, naming the table and both numbers.
`blast_radius_exceeded` caps `purge-orphans` at `BLAST_RADIUS_MAX_RATIO = 0.01` — strictly above 1%
of the scanned population aborts the run.

```
delete_count_match_is_ok                              ✅  sent 5, removed 5 → Ok
delete_count_mismatch_bails_naming_table_and_both_counts ✅ sent 5, removed 3 → Err naming "property", 5 and 3
blast_radius_exceeded_true_strictly_above_threshold   ✅  2 of 100 → exceeded
blast_radius_exceeded_false_at_threshold              ✅  1 of 100 is not *above* 1%
blast_radius_exceeded_false_below_threshold           ✅
blast_radius_exceeded_handles_zero_scanned_without_dividing_by_zero ✅ the pure fn stays total
```

The cap applies to `purge-orphans` only. `purge-ocod-owners` deliberately does **not** carry it:
selecting 100% of the `GB-OCOD:`-sourced rows is that command's correct and intended scope, so a
ratio cap would fire on the success case. Its controls are the typed digest, the operator's
out-of-band backup, and the fail-loud removed-count check.

**`GB-OCOD:` scan scope (pure).** The scan covers *every* `GB-OCOD:`-sourced row, not only
the owners whose key text shows a country variant, and computes one digest over the node and edge
halves together.

```
scan_stale_ocod_node_filter_selects_only_gb_ocod_keys ✅  OffshoreEntity NodeAdded rows keyed GB-OCOD: only
scan_stale_ocod_edge_filter_covers_every_gb_ocod_source_not_just_merged_ones ✅ every GB-OCOD:-sourced OwnsProperty edge
scan_stale_ocod_digest_is_computed_over_nodes_and_edges_together ✅  one digest across both halves
scan_stale_ocod_confirmation_ids_dedupe_a_row_appearing_in_both_halves ✅ a row in both halves is confirmed once
```

**Against a real SurrealDB (4 tests, `URBAN_DB_TESTS=1`).** Each of these exists because the pure
version of the same claim is not sufficient evidence:

```
scan_orphans_migrated_property_scan_discriminates_correctly ✅ the migrated paged scan returns exactly the seeded Property nodes and none of the decoys seeded alongside them
delete_in_batches_reports_real_removed_count_against_seeded_property_rows ✅ the count is what SurrealDB removed, not what the caller sent
delete_in_batches_removes_rows_whose_ids_came_back_quoted_from_the_db ✅ the regression: deletes against an id the DATABASE handed back (backtick-quoted), not one hand-built in the test
delete_in_batches_bails_when_a_row_was_removed_out_of_band_before_the_call ✅ a row deleted between scan and delete is a hard error, not a shrug
```

The third is the one to keep. A test that constructs its own record id proves only that the code
agrees with the test's idea of a key; this one takes the id back out of SurrealDB in the form
SurrealDB prints it, backticks and all, and deletes against that. The live run on
`an_anglesey_aebaa785` aborted on 2026-08-22 with "sent 10000, removed 0" for exactly this reason,
and every hand-built-id test passed throughout.

### `crates/cli/tests/purge_delete_spike.rs` — 1 test, DB-gated

`delete_return_before_yields_real_removed_count` locks the exact SurrealQL and Rust bind shapes
that yield a real removed-count through the `=3.1.0-beta.2` SDK:

```surql
DELETE type::table($table) WHERE id IN $ids RETURN BEFORE
```

bound with `Vec<surrealdb::types::RecordId>` and read via `.take(0)`. Deleting 2 of 3 seeded rows
returns an array of exactly 2 elements, and a follow-up `SELECT count()` shows the 1 row left
behind — proof the array length is evidence rather than a restatement of what the caller sent. It
runs against an isolated `TestNs`, never the live analysis database. Copy this shape rather than
re-deriving it.

### `crates/ingestor/tests/projector_ocod_owner_merge.rs` — 2 tests, DB-gated

The OCOD owner-key fix alone changes nothing in the live graph, because `graph_events` is
**append-only**: re-running the adapter emits a third, correctly-keyed node *alongside* the two
fragmented ones. The merge is only real once the superseded events are retired first. This file
drives the whole cycle through the real `Projector` — seed the two pre-fix country-variant nodes
(`GB-OCOD:germany-fc025391`, `GB-OCOD:hamburg-fc025391`, one live overseas company) and
their ownership edges, run the scan and delete, re-emit the corrected single-key node
(`GB-OCOD:fc025391`) with both edges re-pointed at it, project, and assert what survives.

```
test_purge_then_re_emit_merges_eaubrink_to_one_offshore_entity ✅ exactly one offshore_entity row, both properties resolving to it
test_re_emit_without_purge_leaves_the_old_nodes_behind         ✅ the negative control: skip the purge and you get three rows
```

The negative control is what makes the first test mean something — without it, the purge step is
assumed load-bearing rather than proven so.

**The Cargo dependency cycle.** `crates/ingestor/Cargo.toml` declares
`urban-integrity-cli = { path = "../cli" }` under `[dev-dependencies]`, which makes a cycle:
`cli → ingestor → [dev] cli`. This is deliberate and Cargo permits it *specifically* through
dev-dependencies — dev-deps do not participate in the normal build graph, so there is no cycle to
resolve when building either crate for real. The alternative was to copy the purge scan/delete into
the ingestor's test, which would let the test keep passing against a copy that had drifted from the
shipped code. The test calls
`urban_integrity_cli::commands::purge::{scan_stale_ocod_owner_events, delete_stale_ocod_events}`
directly, so it exercises the same functions the CLI ships.

```bash
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli purge
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test purge_delete_spike
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_ocod_owner_merge
```

---

## Long-Running Job Resilience (`crates/ingestor/src/backfill/`)

A full `backfill ch` pass is multiple hours (~20,000 Companies House lookups at 2 req/s), which is
longer than SurrealDB's ~1-hour HTTP session lifetime. Two modules exist for that, each with its own
tests.

### `session.rs` — 5 tests (1 DB-gated)

```
classifies_the_live_401_as_an_auth_error              ✅  the live `401 Unauthorized for url .../rpc` recognised as an auth error
classifies_server_side_auth_rejections                ✅  server-side auth rejections classified alongside it
does_not_classify_ordinary_query_failures_as_auth_errors ✅  an ordinary query failure does not trigger a re-auth loop
db_query_recovers_from_a_dead_session                 ✅  (DB) a query against an invalidated session re-auths and succeeds
reauth_without_credentials_is_an_error_not_a_silent_noop ✅  missing credentials → Err, never a silent no-op
```

`db_query_recovers_from_a_dead_session` is one of the tests that requires CI's `surrealdb_auth:
true` to mean anything.

### `cursor.rs` — 8 tests (2 DB-gated)

`backfill ch` is not naturally re-entrant: its two negative verdicts (`NoMatch`, `Ambiguous`) leave
the candidate entity unchanged, so a restart re-issues every lookup that learned nothing.

```
resumes_after_an_exact_match                     ✅  resume point respected after a match
resumes_after_a_candidate_that_left_the_set      ✅  resume works when the candidate list shifted underneath
a_cursor_at_the_last_item_leaves_nothing_to_do   ✅  cursor at the end → no work
a_cursor_beyond_every_candidate_leaves_nothing_to_do ✅  cursor past the end → no work, no panic
a_cursor_before_every_candidate_starts_from_the_top  ✅  cursor before the start → full pass
an_empty_candidate_list_is_handled               ✅  empty candidate list → no work, no panic
db_cursor_round_trips_and_clears                 ✅  (DB) persist → load → clear round-trip
db_cursors_for_different_jobs_do_not_collide     ✅  (DB) per-job cursor keys are isolated
```

### `planning.rs` — 37 tests (5 DB-gated)

The planning-applicant repair pass, whose central risk is *duplicate* writes rather than missing
ones: `graph_events` is append-only, so a candidate set chosen by "provenance absent" instead of "the
`AwardedTo` edge still points at the placeholder" would re-insert tens of thousands of edges for
records that were already correct. Tests cover the re-entrancy guard
(`test_reentrancy_guard_inserts_zero_edges_for_already_resolved_contract`,
`test_second_pass_over_same_input_adds_no_edges`), provenance-only mutations, the separation of a
failed relist from an empty one, and the bulk-stamp pass never inserting an edge.

```bash
cargo test -p urban-integrity-ingestor backfill
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor backfill
```

---

## Planning Address Parser (`crates/ingestor/src/address/planning.rs` — 19 tests, no DB)

Turns a planning application's free-text address into the same structured shape PPD stores —
`(postcode, saon, paon, street, locality, town)` — so the two corpora can be compared field by
field. It replaces a flatten-and-regex approach that could not distinguish a SAON from a PAON in
principle.

The first stage splits on the bilingual `" / "` delimiter, because 95.1% of live planning addresses
are a Welsh/English pair written twice. 9 of the 19 tests are named `gap_closure_*` and each pins a
specific real address that the previous parser mishandled (building name mistaken for SAON, bare
number segments, terrace names, PAON ranges). The rest are robustness cases: empty string, no comma
and no postcode, postcode-only input, and multiple bilingual delimiters must all parse without
panicking.

```bash
cargo test -p urban-integrity-ingestor address::planning
```

---

## Registry and Identity Adapters

### FCA Mutuals Public Register (`crates/enricher/src/adapters/mutuals_register.rs` — 6 tests, no DB)

Registered societies (housing associations, co-ops, community benefit societies) hold no Companies
House number, so they were invisible to every CH-keyed adapter.

```
parses_clwydalyn_from_the_real_row                    ✅  a real register row parses to the expected society
deregistered_society_maps_to_dissolved_with_its_date  ✅  deregistered → EntityStatus::Dissolved carrying its date
nameless_stub_rows_are_skipped                        ✅  stub rows without a name are skipped, not half-ingested
cp1252_apostrophe_does_not_truncate_the_file          ✅  a CP1252 apostrophe does not truncate the parse mid-file
postcode_extraction_handles_real_address_shapes       ✅  postcode extracted across the address shapes the register uses
society_entity_carries_no_company_number              ✅  a society is never given a fabricated company_number
```

### Planning applicant → Companies House identity (`crates/enricher/src/adapters/planning_identity.rs` — 19 tests, no DB)

Gives planning applicants a CH identifier by exact-name join, under the workspace exact-match-only
policy. The tests fence both directions: it must link the right entity, and it must refuse anything
short of an exact match.

```
test_name_is_planning_identity                        ✅  adapter.name() == "planning_identity"
test_always_enabled                                   ✅  no credential required → always enabled
test_unknown_applicant_never_a_candidate              ✅  the `director:unknown-applicant` placeholder is never a candidate
test_director_entity_is_a_candidate                   ✅  a director-keyed applicant is a candidate
test_ch_entity_is_not_a_planning_candidate            ✅  an entity that already has a CH key is not a candidate
test_exact_match_produces_link                        ✅  exact normalised-name match → identity link
test_no_match_increments_discarded                    ✅  no match → discarded counter, no link
test_ambiguous_collision_matches_neither              ✅  two entities sharing a normalised name → neither is linked
test_build_name_lookup_no_collision_when_same_entity_repeated ✅  the same entity repeated is not a collision
test_build_name_lookup_skips_empty_normalised_name    ✅  names that normalise to empty are skipped
test_near_miss_name_does_not_match                    ✅  a near-miss name does not link (exact-match-only policy)
test_contains_initial_detects_abbreviated_names       ✅  abbreviated ("J. Smith") names detected
test_contains_initial_ignores_full_names              ✅  full names not treated as abbreviations
test_confidence_for_abbreviated_name_is_below_auto_link_threshold ✅  abbreviated name scores below AUTO_LINK_THRESHOLD
test_confidence_for_full_name_auto_links              ✅  full-name match scores into auto_linked
test_confidence_change_does_not_weaken_exact_matching ✅  the confidence tuning cannot weaken exact matching
```

### Land Registry OCOD (`crates/enricher/src/adapters/land_registry_ocod.rs` — 14 tests, 1 DB-gated) and PPD (`land_registry_ppd.rs` — 14 tests, 1 DB-gated)

```
test_ocod_parse_country                          ✅  proprietor country parsed from the OCOD row
test_canonicalise_before_filter                  ✅  country canonicalised before the jurisdiction filter runs
test_ocod_retains_foreign_numbered_proprietor    ✅  a foreign-numbered proprietor is retained, not dropped
test_ocod_accepts_roe_number                     ✅  Register of Overseas Entities number accepted as an identifier
test_owner_key_forms                             ✅  every supported owner-key form produces a stable key
test_ocod_icij_cap_pending_review                ✅  an ICIJ-corroborated OCOD link is capped at pending_review
eaubrink_merges_on_shared_company_number         ✅  two country spellings of one numbered owner collapse to one key
black_stone_stays_country_keyed_no_number        ✅  an owner with no company number stays country-keyed
fc000000_sentinel_falls_back_to_country_key      ✅  the FC000000 sentinel is not treated as a real number
owner_key_refuses_bare_number_for_a_non_unique_number ✅  a number shared by unrelated owners does not become a merge key
owner_key_still_merges_every_other_genuine_number ✅  the non-unique exclusion does not disturb the general case
is_non_unique_number_is_case_and_whitespace_insensitive ✅  the exclusion list matches regardless of case or padding
is_sentinel_number_structural_cases              ✅  sentinel detection is structural, not a hard-coded list
icij_name_index_pages_and_returns_every_row      ✅  (DB) the ICIJ name index pages — see below

land_registry_ppd_parses_fixture_row             ✅  fixture PPD row parses to a transaction
ppd_filter_excludes_out_of_scope_district        ✅  out-of-scope postcode district excluded
ppd_filter_empty_allowlist_yields_no_records     ✅  empty allowlist → zero records (fail closed, not open)
ppd_adapter_name_is_ppd                          ✅  adapter.name() == "ppd"
ppd_adapter_disabled_by_default                  ✅  disabled unless explicitly configured
ppd_adapter_enabled_when_config_true             ✅  enabled when configured true
ppd_postcode_district_extracts_outward_code      ✅  outward code extracted from the postcode
ppd_normalise_street_lowercases_and_collapses    ✅  street normalisation lowercases and collapses whitespace
normalise_street_ppd_output_unchanged_by_planning_parser_work ✅  the planning parser rewrite left PPD normalisation byte-identical
ppd_idempotent                                   ✅  (DB) re-running the same input does not duplicate rows
```

The owner-key tests above are the unit half of the OCOD merge; the graph half is
`crates/ingestor/tests/projector_ocod_owner_merge.rs`, covered under *OCOD Owner Merge and the
Purge Path*.

`icij_name_index_pages_and_returns_every_row` is worth reading before touching any bulk `SELECT` in
this codebase. `load_icij_name_index` read `icij_offshore_entity` in one unpaged statement. On the
live corpus that table holds 814,315 rows, and the SurrealDB 3.0.2 server / `=3.1.0-beta.2` SDK
pairing cannot decode a response that large — "Failed to deserialize flatbuffers payload" aborted
the whole OCOD re-emit before a single mutation was sent. It had never bitten because the ICIJ
adapter wrote zero rows for its entire life until that defect was fixed, so this loader had only
ever read an **empty** table. The test seeds 5 rows and calls `load_icij_name_index_paged(&db, 2)`
with a deliberately tiny page size, so the multi-page path is exercised without seeding hundreds of
thousands of rows: it asserts all 5 rows come back (a short page is not end-of-data) and that a row
from a later page is present and keyed case-insensitively.

`ppd_idempotent` is the sanctioned way to check PPD idempotency. Do **not** verify it by re-running
a full ingest: a redundant full `enrich --source ppd` pass thrashes SurrealDB's background tasks
(`Transaction conflict: Resource busy`) and can wedge the query engine for hours. Idempotency is
structural (UPSERT on `transaction_id`); confirm it with this test, an unchanged `count()`, or a
one-year re-run.

### Jurisdiction canonicalisation (`crates/enricher/src/country_canonical.rs` — 4 tests, no DB)

```
variants_map_to_one_canonical_token              ✅  spelling/format variants collapse to one token
other_jurisdictions_canonicalise                 ✅  the non-obvious jurisdictions canonicalise too
unknown_string_passes_through_trimmed_never_dropped ✅  an unknown country is trimmed and kept — never dropped
blank_passes_through                             ✅  a blank value passes through without inventing a country
```

The third test is the important one: canonicalisation must never silently discard a jurisdiction it
does not recognise.

---

## Land Flip Detector (`crates/enricher/src/land_flip.rs` — 32 tests, 8 DB-gated)

The land-flip detector pairs a planning application against PPD transactions through three
sequential gates, and each gate has its own negative tests — the detector's failure mode is false
positives, not misses.

```
parse_date_value_accepts_ymd_and_rfc3339         ✅  both date encodings accepted
parse_date_value_rejects_garbage_and_non_strings ✅  garbage and non-strings rejected
naive_to_utc_anchors_at_midnight                 ✅  naive dates anchored at UTC midnight
coerce_str_handles_strings_empty_and_null        ✅  string / empty / null coercion
parse_ppd_txn_parses_full_row                    ✅  full PPD row parsed
parse_ppd_txn_requires_id_and_date               ✅  a transaction without id or date is rejected
parse_ppd_txn_defaults_optional_fields           ✅  optional fields default rather than failing the row
normalise_app_address_excludes_paon_from_street_post_parser ✅  PAON excluded from the street component
normalise_app_address_bare_number_segment_yields_paon_and_clean_street_not_fused_garbage ✅  bare number segment yields PAON + clean street
normalise_street_and_planning_parser_agree_on_ppd_shaped_inputs ✅  both normalisers agree on PPD-shaped input
normalise_app_address_no_postcode_disables_gate1_component ✅  no postcode disables that gate-1 component
normalise_app_address_rejects_too_short_postcode ✅  a too-short postcode is rejected
assemble_app_contexts_joins_located_at_via_contract_not_applicant ✅  LocatedAt joined via the Contract, not the applicant
assemble_app_contexts_does_not_fall_back_to_applicant_keyed_located_at ✅  no fallback to applicant-keyed LocatedAt
gate1_postcode_only_produces_zero                ✅  a postcode-only overlap scores zero
gate1_reject                                     ✅  gate 1 rejects a non-matching address
gate1_saon_is_never_part_of_the_equality         ✅  SAON never participates in the address equality
gate1_flat_number_does_not_manufacture_a_match   ✅  a flat number cannot manufacture a match
gate_all_pass                                    ✅  all three gates passing produces a candidate
candidate_invariants                             ✅  emitted candidates satisfy their invariants
land_assembly_suppression                        ✅  land-assembly patterns suppressed
gate2_null_date_no_proxy_is_none                 ✅  null date with no proxy → None, not a guess
gate2_tier_boundaries                            ✅  gate-2 tier boundaries exact
gate2_tier1_window_closes                        ✅  the tier-1 window closes on schedule
table_scan_reads_past_a_page_boundary            ✅  (DB) the scan crosses a page boundary
postcode_only_match_exists_distinguishes_no_data_from_postcode_overlap ✅  (DB) "no data" distinguished from "postcode overlap"
gate3_no_link_rejects                            ✅  (DB) gate 3 rejects when the link is absent
gate3_positive_link_produces_candidate           ✅  (DB) a positive link produces a candidate
```

```bash
cargo test -p urban-integrity-enricher land_flip                    # 32 in land_flip.rs — 24 pure-logic + the 8 DB-gated ones returning at the gate; re-measure the filtered total
URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher land_flip   # historical: 28 passed in 1.10s when the file had 28 tests (4 gated); it now has 32 (8 gated) — re-measure
```

---

## Schema Migration (`crates/cli/src/commands/migrate.rs` — 4 tests, 2 DB-gated)

```
test_migration_ddl_properties                    ✅  the migration DDL is idempotent and well-formed
reauth_precedes_every_post_preflight_db_query    ✅  source-text assertion: every `db.query(...)` after the pre-flight is preceded by a `reauth_db(` call
reauth_db_recovers_from_a_genuinely_expired_token ✅  (DB) recovery from a genuinely expired token, not a simulated one
located_at_accepts_contract_via_schema_def_and_migration_ddl ✅  (DB) LocatedAt accepts a Contract endpoint under both SCHEMA_DEF and the migration DDL
```

```bash
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli commands::migrate
```

---

## Alert Read Path (`crates/core/src/persistence/queries.rs` — 6 pure tests, no DB)

The alert read path hid this project's only live `PhoenixLoop` alert for a week: `alerts list`
ordered by `timestamp DESC`, filtered inside a `limit × 10` recency window rather than over the
corpus, and silently clamped the limit to 1,000. The tests that were supposed to catch that asserted
against a re-implementation of the filter *written in the test itself*, so an ordering bug survived
them — a reminder that a test which restates the implementation tests nothing.

The filtering logic now lives in a pure `select_alerts(records, filter) -> AlertPage`, which is
unit-testable without a DB. Each of these was verified to **fail** against the old timestamp
ordering before being committed (`5a95ba9`):

```
the_highest_scoring_alert_comes_first_however_old_it_is    ✅  score DESC beats recency
a_motif_filter_finds_a_match_buried_under_newer_alerts     ✅  the filter runs over the corpus, not a window
the_page_reports_how_many_matched_not_just_how_many_are_shown ✅  AlertPage carries "N of M"
an_unlimited_query_returns_everything_and_is_not_truncated ✅  limit 0 → no clamp, truncated() == false
ordering_is_deterministic_when_scores_and_dates_tie        ✅  id breaks the final tie, so the order is total
a_withdrawn_alert_is_excluded_unless_explicitly_requested  ✅  withdrawn hidden by default
```

```bash
# The six live in persistence::queries::tests — no DB required.
cargo test -p urban-integrity-core --lib persistence::queries::tests::the_highest_scoring
cargo test -p urban-integrity-core --lib persistence::queries          # the whole 62-test module
```

> **Writing tests here:** assert against the *behaviour* (order, counts, membership), never against
> a filter predicate reconstructed in the test. If the test has to re-derive the answer the way the
> code does, the two agree by construction and neither is checked.

---

## Officer Identity (`crates/ingestor/src/backfill/officers.rs` — 23 tests, no DB)

`backfill officers` keys officer nodes on the identity evidence Companies House actually publishes —
the officer id and the `{month, year}` date of birth — rather than on the name. The tests encode
what that evidence does and does not support, which is the whole point of the module:

```
a_date_of_birth_produces_the_established_suffix           ✅  DOB present → the established key suffix
no_date_of_birth_leaves_the_key_bare                      ✅  no DOB (e.g. a secretary) → bare name key
dates_of_birth_separate_two_namesakes                     ✅  two people, one name → two keys
officer_ids_that_agree_on_a_date_of_birth_are_one_person   ✅  agreement is fusion evidence, not collision
two_distinct_officer_ids_on_one_key_are_split_not_fused    ✅  disagreement splits
one_person_on_many_boards_is_not_a_collision               ✅  many appointments ≠ many people
one_officer_on_two_boards_is_not_reported                  ✅  the audit does not report it
a_key_carrying_an_unidentifiable_appointment_is_refused    ✅  refuses rather than guessing
an_unevidenced_appointment_is_visible_as_such              ✅  reported, not absorbed
the_resolver_refuses_rather_than_falling_back_to_the_bare_name ✅ no silent name fallback
the_resolver_returns_the_key_each_appointment_was_planned_under ✅ plan and apply cannot diverge
a_corporate_role_is_carried_through_from_the_cache         ✅  officer_role survives the backfill
every_live_corporate_role_matches_phoenixs_prefix_test     ✅  all four live corporate-officer role strings match the detector's `corporate-` prefix guard
the_real_officer_cache_keys_every_collision_on_evidence    ⏭  #[ignore] — every non-empty line of the real on-disk cache parses; run with --ignored
an_edge_is_attributed_by_its_company                       ✅  split re-keying: edges follow the company
a_node_without_a_company_is_attributed_by_date_and_role     ✅  fallback attribution for bare nodes
an_unattributable_node_is_reported_not_guessed              ✅  no company, no date → reported
two_appointments_at_one_company_take_one_event_each         ✅  no event duplication on re-key
events_on_unaffected_keys_are_untouched                     ✅  the rewrite is scoped
officer_ids_agreeing_on_a_date_of_birth_are_not_reported_even_when_the_node_links_both ✅
an_uncorroborated_key_linking_both_officers_companies_is_fused ✅  a genuine fusion is called one
a_colliding_key_touching_one_officer_only_is_not_yet_fused  ✅  collision ≠ fusion
fused_keys_are_reported_before_merely_colliding_ones        ✅  audit ordering by severity
```

```bash
cargo test -p urban-integrity-ingestor backfill::officers    # no DB, no network
```

> `--split-collisions` rewrites `graph_events` — it is a data correction, not a read, and **no test
> covers the write**. `officers.rs` contains zero DB-gated tests (verified 2026-08-23: no
> `URBAN_DB_TESTS`, `db_tests_enabled()` or `skip_if_no_db()` anywhere in the file); all 23 tests
> above are pure functions over in-memory batches. The planning and application of the re-key are
> tested; the SurrealDB write that applies it is not. Run `--dry-run` first, and `project`
> afterwards.

### The second silent skip: fixture files that are not in the repository — now closed

`the_real_officer_cache_keys_every_collision_on_evidence` reads
`data/ch-officers-cf-awardees.jsonl`, and `the_real_python_cache_parses_without_dropping_a_line`
(in `cessation.rs`) reads `data/ch-company-status-cache.jsonl`. **`data/*` is gitignored**, and
neither file is tracked, so on a fresh clone neither fixture exists.

Until `158ab71` both tests opened the file with a let-else that printed `SKIP:` and returned. On a
fresh clone they therefore returned immediately and reported as **passing**, exactly like the DB
gate — but with no environment flag to turn them on. They are the strongest tests in these two
modules (they assert that every non-empty line of the real corpus parses, because a dropped row is
silent data loss), and they were the two most likely to be silently absent.

Both now carry an explicit `#[ignore]` naming the file they need:

```rust
#[ignore = "needs the operator's local data/ch-officers-cf-awardees.jsonl; run with --ignored"]
```

and, past that gate, `read_to_string(path)` **panics** rather than skipping — reaching that line
means someone asked for the test explicitly and is owed a real answer. So the two states are now
distinguishable in the summary line: `ignored` on a machine without the caches, `ok` on one with
them. They account for 2 of the 11 ignored in a default `cargo test --workspace`.

```bash
# On a machine that has run the backfills that produce those caches:
cargo test -p urban-integrity-ingestor -- --ignored the_real_officer_cache_keys_every_collision_on_evidence
cargo test -p urban-integrity-ingestor -- --ignored the_real_python_cache_parses_without_dropping_a_line
```

`data/party_authority_control.json` is the counter-example done right: it is force-added to git
(`git ls-files data/` shows exactly three tracked files — it, `country_incorporation_canonical.json`
and `formation_agent_postcodes.txt`), so the donation-correlation tests that read it use `.expect()`
and fail loudly if it goes missing, rather than skipping.

---

## Cessation Dates (`crates/ingestor/src/backfill/cessation.rs` — 17 tests, no DB)

Phoenix's window is anchored on when a company actually failed. The graph's own dissolution dates
are frequently the day the bulk stream *observed* the failure, not the day it happened, so the
backfill has to tell a real date from a stamp:

```
a_date_equal_to_its_own_write_day_is_a_stamp              ✅  observation date, not a cessation date
a_bulk_batch_date_written_later_is_real_not_a_stamp        ✅  the converse case
a_historical_date_is_real                                  ✅
an_uncorrected_stamp_is_still_targeted                     ✅
a_later_correction_removes_the_entity_from_the_target_set  ✅  idempotent across re-runs
stamped_dates_are_processed_before_coverage_checks         ✅  ordering of the two target classes
only_companies_house_entities_are_targets                  ✅
mutuals_register_entities_never_enter_the_target_set       ✅  GB-MUT: societies have no CH record
an_active_company_is_a_coverage_check                      ✅
in_liquidation_is_stamp_checked_too                        ✅
receivership_and_administration_count_as_ceased            ✅
reads_the_python_scripts_cache_shape                       ✅  reads the cache the older script wrote
the_real_python_cache_parses_without_dropping_a_line       ⏭  #[ignore] — every non-empty line of the real on-disk cache parses; run with --ignored
a_truncated_final_line_does_not_abort_the_load             ✅  a half-written last line is skipped, not fatal
a_row_without_a_timestamp_is_dropped_not_treated_as_the_epoch ✅ missing ≠ 1970
applying_a_profile_preserves_every_other_field             ✅  the update is surgical
a_profile_with_nothing_to_add_produces_no_update           ✅  no-op writes no event
```

```bash
cargo test -p urban-integrity-ingestor backfill::cessation
```

---

## Jurisdiction Preflight

A deprivation index for the wrong nation loads cleanly, matches zero wards, and leaves the
weighting inert while appearing configured. The preflight exists to make that a startup refusal
rather than a silent no-op, and the tests are spread across three crates:

**`crates/schema/src/nation.rs` — 7 tests, no DB**

```
ward_codes_map_to_their_nation                        ✅  E05/W05/S13/N08 → England/Wales/Scotland/NI
every_gss_level_carries_the_same_nation_letter        ✅  the nation letter is stable across GSS levels
malformed_codes_are_rejected_rather_than_guessed      ✅
case_and_surrounding_whitespace_do_not_change_the_answer ✅
jurisdiction_covers_only_the_nations_it_declares      ✅
jurisdiction_describes_itself_for_the_operator        ✅  the refusal message names the mismatch
nation_parses_from_an_operator_supplied_name          ✅
```

**`crates/core/src/config.rs` — 8 of its 31 tests cover the preflight, no DB**

```
analysis_nation_derives_from_configured_wards         ✅  nation inferred from [ingest] ward codes
mixed_nation_wards_are_refused                        ✅  a two-nation analysis is an error
mixed_nations_can_be_allowed_deliberately             ✅  ... unless explicitly opted into
an_explicit_nation_covers_an_analysis_with_no_wards   ✅
an_explicit_nation_that_contradicts_the_wards_is_refused ✅  config and data must agree
an_unparseable_ward_code_is_an_error_not_a_guess      ✅
an_unparseable_nation_name_is_an_error                ✅
no_wards_means_no_nation                              ✅  absence is not England
```

**`crates/enricher/tests/jurisdiction.rs` — 5 tests, no DB**

```
land_registry_adapters_declare_england_and_wales      ✅  HMLR covers E+W only
charity_commission_declares_england_and_wales         ✅
adapters_default_to_uk_wide                           ✅  an undeclared adapter is not silently narrowed
out_of_jurisdiction_adapters_are_refused_admission    ✅  a Scottish analysis does not run HMLR
unknown_analysis_nation_admits_every_adapter          ✅  unknown → permissive, and says so
```

```bash
cargo test -p urban-integrity-schema nation
cargo test -p urban-integrity-core config
cargo test -p urban-integrity-enricher --test jurisdiction
```

---

## Anomaly Surface

An anomaly records an input the pipeline **refused to guess at**. It is deliberately not an alert
and not a log line: alerts are a corruption queue, and a log line is discarded. The tests are thin
because the surface is thin — the load-bearing behaviour is that counting is exact even when
recording is sampled.

**`crates/ingestor/src/backfill/anomaly_sink.rs` — 2 tests, no DB**

```
recording_is_sampled_but_counting_is_not              ✅  the count is a true total; the stored rows are a sample
the_first_drops_are_always_recorded                   ✅  the first occurrences are never sampled away
```

**`crates/cli/src/commands/anomalies.rs` — 1 test, no DB**

```
a_key_containing_colons_survives_prefix_stripping     ✅  only the `data_anomalies:` table prefix is stripped
```

That last one guards a real hazard: anomaly keys legitimately contain colons (they are built from
the source and the subject, e.g. `officer_key_collision_backfill:officers:audit_director:…`), so a
naive "split on the first colon" lookup truncates the key and finds nothing.

> **Known gap, and it is a testing gap too:** `data_anomalies` has no retraction path, so nothing
> asserts that an audit which *stops* reporting a finding clears the old `open` row. It does not.

---

## Status Fold and Projection Markers

Two small DB-gated files that exist because the projected tables silently disagreed with the hot
graph for most of this project's life — status corrections written after a node first appeared were
dropped on read and on projection.

**`crates/core/tests/entity_query_status_fold.rs` — 3 tests, all DB-gated**

```
test_query_entity_by_id_folds_nodeupdated_status          ✅  (DB) id lookup folds NodeUpdated over NodeAdded
test_query_entity_by_ch_number_folds_nodeupdated_status   ✅  (DB) same for the CH-number path
test_query_entity_by_name_folds_nodeupdated_status        ✅  (DB) same for the name path
```

All three read paths are tested separately on purpose: they are three different queries, and fixing
one historically left the others stale.

**`crates/ingestor/tests/projector_run_marker.rs` — 2 tests, both DB-gated**

```
completed_run_stamps_the_marker                       ✅  (DB) a finished projection is recorded as finished
an_unfinished_marker_is_replaced_by_the_next_run      ✅  (DB) a crashed run does not masquerade as complete
```

**`crates/core/tests/detector_run_marker.rs` — 2 tests, both DB-gated.** The same
marker discipline for detection rather than projection: `precomputed-alerts` reads
`detector_runs:current` to tell "the detector ran and found nothing" from "the detector never ran",
so the marker's absence has to mean exactly one of those.

```
detector_run_marker_written_when_detector_ran         ✅  (DB) a bake with a detector configured writes detector_runs:current with the right motif_count
detector_run_marker_absent_when_no_detector_configured ✅  (DB) a bake with detector: None leaves detector_runs untouched — no row, not even motif_count: 0
```

**`crates/ingestor/tests/projector_declares_interest.rs` and `projector_disqualification.rs` — 1
test each, both DB-gated.** Two edges that had never once reached the graph, for the same underlying
reason: the relation's `FROM`/`TO` types could not accept the endpoints the adapter emitted, so
SurrealDB refused the `RELATE` and the projected table stayed permanently empty. Neither failure was
visible, because the adapters producing them are not enabled by default — zero output was
indistinguishable from "nothing found".

```
test_declared_interest_materialises_as_a_relation_row     ✅  (DB) a councillor's declared company interest gets its own relation rather than being forced through `directs` (which is FROM entity|officer, so a political_actor src was rejected outright)
test_disqualification_materialises_as_node_and_relation   ✅  (DB) a CDDA disqualification is modelled as its own node kind — the old `DisqualifiedFrom` edge pointed at a `disq:{resource_id}` dst that no AddNode ever created and NodeWeight had no variant for
```

```bash
URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test entity_query_status_fold
URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test detector_run_marker
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_run_marker
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_declares_interest
URBAN_DB_TESTS=1 cargo test -p urban-integrity-ingestor --test projector_disqualification
```

---

## Adapter Endpoint Regressions (DB-gated)

Two small files that pin behaviour a unit test cannot reach, because the defect was in the query
the adapter sends rather than in the logic around it:

**`crates/enricher/tests/donation_correlation_endpoint.rs`**

```
a_donor_with_a_contract_in_window_fires_an_alert      ✅  (DB) end-to-end through the real query
an_unrelated_contract_does_not_correlate              ✅  (DB) the negative case, same path
a_donor_with_two_contracts_in_window_fires_one_alert  ✅  (DB) two in-window contracts produce one alert, not two
```

**`crates/enricher/tests/opencorporates_targets.rs`**

```
test_target_query_excludes_entities_without_a_company_number ✅  (DB) no CH number → not a target
test_is_not_null_does_not_filter_absent_fields               ✅  (DB) SurrealDB's IS NOT NULL does not exclude *absent* fields — the trap this test names
```

The second is worth reading before writing any SurrealQL filter in this codebase: in SurrealDB,
`IS NOT NULL` matches a row whose field is missing entirely. A filter written on the assumption that
it does not will quietly include exactly the rows it was meant to exclude.


---

## Probe Battery (`crates/probe/`)

The forensic probe battery lives in its own crate, `urban-integrity-probe`, so that both the CLI
`probe` subcommand and the workbench server can run it in-process. It used to live under
`crates/cli/src/commands/probe/`; some test headers and older notes still name that path. The
crate has **239 test functions, 127 of them DB-gated** (static census, `d828be7f`), plus CLI-side
end-to-end and static-scan binaries that exercise it from outside.

| Location | Tests | DB-gated |
|----------|------:|---------:|
| `crates/probe/src/*.rs` — the 22 probe modules | 147 | 117 |
| `crates/probe/src/graph.rs` — dependency graph and gating | 18 | 0 |
| `crates/probe/src/lib.rs`, `run.rs`, `population_source.rs` | 12 | 0 |
| `crates/probe/src/report/**` — evidence-report renderers | 43 | 0 |
| `crates/probe/tests/planted_evidence_refs.rs` | 10 | 10 |
| `crates/probe/tests/findings_coverage.rs` | 5 | 0 |
| `crates/probe/tests/motif_kind_coverage.rs` | 3 | 0 |
| `crates/probe/tests/golden_report.rs` | 1 | 0 |

The rows sum to the crate total. Per-module figures for the gated modules are in the census table
under *How to count the DB-gated tier*.

**The dispatch tests (`lib.rs`, no DB)** pin the shape the battery is required to have: one ordered
slice of probes, one `&mut Strategy` threaded through every call, and no verdict-conditioned
branching in the loop.

```
two_stub_probes_thread_a_shared_strategy         ✅  state written by one probe is visible to the next
every_probe_runs_regardless_of_prior_verdict     ✅  a Hit or Blocked upstream does not short-circuit the rest
dispatch_preserves_registration_order            ✅  findings come back in registration order
strategy_enrichment_runs_memo_is_order_independent ✅  the shared memo does not depend on dispatch order
depends_on_defaults_to_empty_when_not_overridden ✅  a probe declares no upstream unless it says so
```

**The per-probe tests** are mostly verdict-boundary tests, and the boundaries are the point: a probe
must distinguish "looked, found nothing" (`Clear`) from "could not look" (`DataGap`) from "a
prerequisite failed" (`Blocked`). Conflating those is what turns a broken pipeline into a clean bill
of health. Representative examples:

```
precomputed_alerts_no_marker_with_alerts_present_is_hit     ✅  (DB) no detector_runs marker but motifs present → Hit
precomputed_alerts_no_marker_zero_motifs_is_clear_without_... ✅  (DB) no marker and 0 motifs → Clear, and the summary says the detector_runs table is empty
precomputed_alerts_marker_present_zero_motifs_is_clear_...  ✅  (DB) marker present, 0 motifs → Clear, and the summary says the detector ran
shared_exact_address_zero_population_is_data_gap_not_clear  ✅  (DB) an empty population is a gap, not an all-clear
shared_exact_address_blocked_fires_before_clear_when_...    ✅  (DB) identity_blocked outranks a Clear verdict
shared_exact_address_blocked_outranks_zero_population_...   ✅  (DB) Blocked outranks DataGap too — the ordering is total, not pairwise
ward_concentration_duplicate_edge_is_deduped_not_double_... ✅  (DB) a repeated Represents edge counts once
detector_readiness_zero_political_actor_nodes_names_...     ✅  (DB) names the motif and the missing term rather than reporting "not ready"
identity_integrity_catalog_baseline_round_trip_not_mocked   ✅  (DB) the baseline goes through the real `_catalog` registry, not a stub
```

**The crate's integration tests:**

- `planted_evidence_refs.rs` (10, DB-gated) — seeds the planted fixture with `seed_planted` and
  asserts the number of evidence references each finding carries against the manifest's literal
  expectations. See *The Planted Fixture and the Toy Analysis*.
- `findings_coverage.rs` (5, no DB) — reconciles the coverage sentence the entity profile states
  about itself ("searched N of M probes") against the probes that actually exist and the evidence
  scope each one declares.
- `motif_kind_coverage.rs` (3, no DB) — reconciles every probe's `motif_kind()` answer against the
  pinned classification table, so a new probe cannot ship without a classification.
- `golden_report.rs` (1, no DB) — a golden Markdown fixture of `render_evidence_report` covering all
  seven `Verdict` kinds (Hit, Weak, Clear, NotApplicable, DataGap, Blocked, Skipped); the report
  writer's output must match it byte for byte, timestamps stripped.

**The CLI-side end-to-end tests (`crates/cli/tests/probe_tier1_e2e.rs`, 3 tests)** cover the two
things a per-probe unit test structurally cannot:

```
probe_battery_returns_seven_findings_with_correct_names_and_nonempty_summaries  ✅  (DB) all 7 Tier-1 probes are reachable through the real ordered battery and return non-placeholder findings
identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact   ✅  (DB) IdentityIntegrity sets Strategy.identity_blocked ITSELF (seeded with zero planning contracts), and SharedExactAddress reads it — proving the cross-probe dependency survives real dispatch, not just a hand-built Strategy
tier1_probes_issue_no_raw_queries                                              ✅  source scan of crates/probe/src (no DB): no probe module contains `.query(` before its first `#[cfg(test)]` marker — probes must route through persistence::scan / registry primitives; asserts the walk saw at least one file so a broken walk cannot pass silently
```

The e2e file builds an `AppConfig` value directly and passes it to the same connect and
analysis-resolution functions that `execute()` calls, rather than going through `execute()`
itself. `execute()` loads configuration from the environment, and redirecting that at a `TestNs`
namespace would mean mutating process-wide env vars — unsafe under `cargo test`'s parallel
execution, where one test's mutation races every other DB-gated test in the binary. This is the same
pattern `analysis_lifecycle.rs` established.

`tier1_probes_issue_no_raw_queries` pairs with `probe_subcommand_source_is_in_scope` in
`query_shape_guard.rs`: one asserts the probes issue no SurrealQL of their own, the other asserts
that if they ever do, the row-losing-shape guard will see it. The scan reads only the portion of
each file before its first `#[cfg(test)]` marker — the probes' own test fixtures do issue raw
queries to seed data, and are meant to.

```bash
# The whole crate, ungated — the 127 DB-gated tests return at the gate
cargo test -p urban-integrity-probe

# The same tests, actually exercised
URBAN_DB_TESTS=1 cargo test -p urban-integrity-probe

# End-to-end battery from the CLI side
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test probe_tier1_e2e
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test probe_tier2_e2e
```

The first pair is the cheapest live demonstration of the skip-by-return gate: identical summary
lines, a large difference in duration. Measure the gated half before citing a figure.

---

## Verdict-Gated Probe Orchestration

A dependency graph sits in front of the probe battery: a Tier-2 probe whose declared upstream did
not produce a usable verdict is not run at all, and returns a synthesised `Verdict::Skipped` finding
naming its direct upstream. Three test surfaces cover it.

### `crates/probe/src/graph.rs` — 18 tests, no DB

14 `#[test]` plus 4 `#[tokio::test]`, all ungated (the file contains no `URBAN_DB_TESTS`,
`db_tests_enabled()` or `skip_if_no_db()` reference at all — the orchestration logic is pure, so its
tests need no store).

```bash
cargo test -p urban-integrity-probe --lib graph
```

They cover four things:

- **`validate_dependency_graph`** — an edge naming an unregistered probe is rejected; a two-probe
  cycle is rejected; the real shipped battery validates; independent probes keep registration
  order; a declared edge that violates registration order **genuinely reorders dispatch** (not
  merely "is accepted").
- **`reject_interleaved_tiers`** — a battery interleaving Tier-1 and Tier-2 is rejected; both-tiers-
  correctly-ordered and Tier-2-only batteries are accepted; and on the real battery, every Tier-1
  dispatch position precedes every Tier-2 position.
- **`gate`** — returns `Ok` when the upstream is `Hit` or `Weak`; returns `Err(Verdict::Skipped)`
  for *each* non-passing upstream verdict, enumerated rather than sampled; a Tier-2 probe with no
  declared dependency gates via the default rule or a named exception; and skips **cascade
  transitively**, each hop naming its own direct upstream rather than the root cause.
- **`ensure_built_for` / `run_gated`** (the 4 async tests) — `run_gated` refuses a graph built for a
  shorter slice and refuses a same-length-but-reordered slice; it **never invokes `run` on a gated-
  out probe**; and its output order matches the input slice order, not the internal dispatch order.

That last pair is the load-bearing one. `run_gated` dispatches in topological order internally but
writes into slots indexed by original position, so the downstream `probes.iter().zip(findings)`
keeps working unchanged. A test that only checked the *set* of findings would not catch a regression
there.

### `crates/cli/tests/probe_oracle_agreement.rs` — 2 tests, both DB-gated

```bash
URBAN_DB_TESTS=1 cargo test -p urban-integrity-cli --test probe_oracle_agreement
```

- `probe_oracle_agrees_on_every_gated_probe_within_delta` — seeds a fixture with no duplicate or
  re-ingested events, so every checked probe's self-declared population and the independent oracle
  count agree exactly. Every row is asserted.
- `probe_oracle_flags_a_real_counting_shape_disagreement` — the non-vacuity proof. It seeds a
  **real** counting-shape disagreement (`address-cluster-exact`'s documented src-parse asymmetry)
  and proves `compare()` detects and flags it, rather than vacuously always agreeing.

**Both tests additionally assert that no entry carries `Verdict::Skipped`.** That assertion holds
*by construction*: both call a local `run_battery`, which calls `urban_integrity_probe::run_probes`
— the deliberately ungated primitive — directly. There is no `ProbeArgs`, no `run::execute()` and
no CLI parsing anywhere in this file's call path, so `graph::run_gated` is never reached. Read the
assertion as "this battery is unaffected by gating", not as "a `--full-battery` flag was passed".

### `crates/cli/tests/probe_report_statics.rs` — 4 tests, no DB

A durable static source scan over `crates/probe/src/` **including its `report/` subdirectory** — a
recursive tree walk. It opens no DB connection, so it shows no gated/ungated duration delta.

```
paren_s_detector_catches_the_defect_and_ignores_ordinary_rust_syntax  ✅  exercises the detector helper against in-test fixtures before the scan is trusted
no_mechanical_pluralisation_in_probe_or_report_modules                ✅  no `(s)` in report prose — string-literal-aware, so `Some(s)` / `Ok(s)` / turbofish `(s)` call sites do not false-positive
sort_unstable_scan_catches_real_calls_and_ignores_comment_mentions    ✅  exercises the `sort_unstable` detector against fixtures, including comment mentions
no_hashmap_iteration_or_sort_unstable_in_probe_or_report_modules      ✅  determinism: no `HashMap` iteration and no `sort_unstable` in report-rendering paths
```

The determinism half is the point: report output must be byte-stable across runs, and `HashMap`
iteration order and `sort_unstable`'s unspecified tie-breaking are the two ways it silently is not.

Both scans are **detector-first**: each ships a paired test that exercises the detection helper
against small in-test fixtures before the helper is trusted to run over the real tree. A blind
`production.contains("(s)")` was tried first and does false-positive — several `Some(s)`/`Ok(s)`/
turbofish call sites existed in probe production code, none of them the defect. A source scan whose
detector is itself untested is a scan that can pass by being broken.
