# Canonical Graph Population

This document records the decision naming the single canonical graph population that every
export, canvas, and in-UI graph-analysis surface in the v9.0 Fraud Analyst Workbench must use —
never re-derive its own count or its own reachability set independently. It exists so a later
phase does not reopen a question this phase already settled: which nodes and edges are "the
graph" when a screen needs to show, export, or algorithmically walk all of it.

Each entry below carries a fixed set of fields — Decided, Producer, Generation timestamp field,
Why, Consumed by — followed by a free-text narrative, matching the field discipline
`docs/ADJUDICATIONS.md` uses for the bucket-(c) differential findings.

## The canonical population

- **Decided:** 2026-09-04, Phase 42 (plan 42-02)
- **Producer:** `crates/cli/src/commands/export_graphml.rs`'s ward-reachability oracle pipeline,
  run in this exact order:
  1. `load_from_events(&config, db_name)` — loads the full graph from `graph_events` via the raw HTTP
     `/sql` endpoint (chosen over the SDK because SurrealDB's flatbuffers codec can fail to
     decode typed `RecordId` fields on large result sets; over plain HTTP JSON every id,
     including coerced `director:` keys, arrives as a string and reconstructs reliably).
  2. `effective_ward_codes(&flag, &config)` — resolves the seed ward/LPA code set: either the
     explicit `--reachable-from-wards` codes, or (bare flag, no codes) every `ward_code` and
     every present `lpa_code` across `config.ingest.planning_portals`, deduplicated and sorted.
  3. The ward-reachability walk itself — the population oracle shipped in v8.0 Phase 37
     (`--reachable-from-wards`, edge dedup, byte-identical reruns, fail-loud on zero seeds).
  4. `filter_graph(graph, node_types, edge_types, drop_isolated)` — applies any `--node-type` /
     `--edge-type` / `--drop-isolated` narrowing on top of the reachable set. Nodes are filtered
     before edges (`petgraph::filter_map` drops any edge whose endpoints did not survive), which
     is why an edge kept by `--edge-type` can still disappear if its endpoints were filtered
     away — deliberate, not a bug, and callers reusing this pipeline must preserve the order.
- **Generation timestamp field:** the canonical population is not a static number — it is the
  output of a run of the pipeline above, and that run has a wall-clock completion time. Every
  surface that renders the population MUST display that time explicitly (the workbench renders
  `population.generated_at` through its `Timestamp` component — relative time plus the absolute
  instant — beside the population figure on the home page's Graph card and the graph page's
  population panel; there is no literal "Population generated" label). A population rendered without this timestamp is indistinguishable
  from a stale cached figure, which is exactly the ambiguity this record exists to close.
- **Why:** v8.0 Phase 38 closed the "a probe reports a verdict over a population it never
  measured" defect class at the probe layer with the population oracle (`ORC-05..08`,
  `probe-oracle`). That closure was scoped to probes. The workbench UI is a new consumer of
  "the graph" that did not exist when the oracle shipped, and without a recorded, single
  canonical pipeline, a UI author could plausibly re-derive their own "the whole graph" query
  (e.g. an unfiltered `load_from_native` scan) that quietly diverges from the oracle-scoped
  population every verdict on screen is described against. This record exists so the UI layer
  does not reopen that defect class independently, the same way the oracle closed it for probes.
- **Consumed by:** Phase 46 (Parquet export), Phase 47 (Cosmograph canvas), Phase 48 (in-UI
  graph analysis). In practice the workbench does not call this pipeline:
  `crates/workbench/src/graph/population_scope.rs` (Phase 44) re-implements the
  ward-reachability walk natively over `CsrSnapshot` (`find_ward_seeds` + `build`), and
  `crates/workbench/tests/population_equals_export.rs` is the DB-gated proof that its node set
  equals `export-graphml --reachable-from-wards`'s for the same fixture. Any new consumer must
  scope to that proven-equal population, never a "give me everything" query of its own.

**Scope note for Phase 42 itself:** this phase computes a population figure for its own status
page via a narrow, intra-crate reuse of the same pipeline calls (`load_from_events`,
`effective_ward_codes`, `resolve_ward_seeds` + `filter_reachable`), wired up as
`canonical_population_summary` in plan 42-07. That is reuse of the existing pipeline's logic
within `crates/cli`, not a new shared abstraction — the cross-crate extraction of this pipeline
into a function `crates/workbench` (or a future shared crate) can call directly, so that
Phases 46–48 are not each re-implementing "call the CLI's internal functions" by hand, is **not**
done here, and it was never done later either: Phase 44 took the native re-implementation path
in `crates/workbench/src/graph/population_scope.rs` instead (see Consumed by above). A future reader must not assume that
extraction already happened just because this record exists — it names the pipeline and its
order, not a already-shared function signature.

Two workbench-specific deviations from the CLI export's policy, both deliberate (Phase 42 code
review, WR-04): (a) the seed ward set is the **target analysis's own** `_catalog` row
`ward_codes`, falling back to `config.ingest.planning_portals` only for the uncatalogued `main`
database — `workbench --analysis <other>` must walk *that* analysis's wards, not whatever
`config.toml` currently names; and (b) zero resolvable seeds is a typed "population
unavailable" state (`PopulationView.count = null` plus `unavailable_reason`), rendered as such
on the home page and graph page, not a boot failure and never a fabricated `0`. The export keeps its
fail-loud zero-seed error.

**Two reads of `graph_events` at workbench boot (Phase 42 code review, IN-09 — recorded, not
fixed):** because the pipeline above lives in `crates/cli` and `crates/workbench` cannot depend
on it, `urban-integrity-cli workbench` reads the whole `graph_events` table twice on every start:
once into the pipeline's own petgraph for the reachability walk (producing
`population.generated_at`), and once more via `replay_from_surreal` into the coordinator's
storage (producing `graph_as_of` / `replayed_at`). Both timestamps travel in the workbench
DTOs, but only `population.generated_at` is rendered in the UI (`graph_as_of` is not displayed
anywhere in `crates/workbench-web`); the two are *expected* to differ by roughly the duration of
the second read, and a reader should not take the pair for a single snapshot. Removing the second read is the same cross-crate extraction named in the scope note
above: once the reachability walk can run over the coordinator's replayed storage from inside
`crates/workbench`, the population can be derived after replay from the one copy already in
memory and both fields stamped from it. That is the Phase 46+ target, not a Phase 42 change.

## Cosmograph interop outcome (WBX-07)

Recorded in the maintainers' Cosmograph spike notes (not part of the published repository). In
short: the workbench loads Cosmograph through the committed, pinned bundle
`crates/workbench-web/js/cosmograph-shim.js` — see `crates/workbench-web/js/README.md`.
