# Projection: the model

Projection is at the heart of UrbanIntegrity, and three things about it are commonly assumed
that are not true. This page states the model in one page; the implementation detail lives in
[ARCHITECTURE.md → CQRS Projection Layer](ARCHITECTURE.md#cqrs-projection-layer).

The short version: **`graph_events` is the source of truth, and the projected tables are a
derived view that is rebuilt destructively.** Both statements have consequences that bite.

## The three misconceptions

**1. `graph_events` is an append-only event log, not a columnar store.** One row per event.
The domain data lives inside `weight_json` as an opaque **JSON string**:

```json
{ "id": "graph_events:0002vaqq7p9yfehk7bsz",
  "event_id": "5c6aae2ded…", "timestamp": 1784823507139, "kind": "NodeAdded",
  "payload": { "entity_id": "location:ordiwedd,-ffordd-cynlas,-benllech",
               "weight_json": "{\"type\":\"Location\",\"name\":\"Ordiwedd, Ffordd Cynlas, Benllech\"}" } }
```

The table is SCHEMALESS with exactly two indexes (`kind`, `timestamp`). Nothing inside
`weight_json` is indexed or queryable. Practical consequence, and it has produced two false
root causes on this project: **a SurrealQL path into the payload returns 0 rows rather than an
error.** `payload.weight.some_field` is not a mistake SurrealDB will report — it silently
matches nothing. Query `payload.weight_json` and parse client-side.

**2. The log is event-sourced for replay and audit, not for speed.** For querying it is
strictly *worse* than tables — you cannot filter, join or traverse on anything in the payload.
That cost is the reason the projector exists, not an argument against the design: append-only
means any past state can be reconstructed, and nothing is ever overwritten in place.

**3. The projected tables are the machine read path, not a human convenience view.**
`project` materialises the log into **11** typed node tables and **12** `TYPE RELATION` tables so
SurrealQL graph traversal works. (This said 10 and 11 until 2026-09-03; both were already wrong
when written — `Disqualification` and `declares_interest` landed 2026-08-23, two days before this
document's last edit. `SCHEMA_DEF`'s own test asserts the 12 at `db.rs:614-616`, and
`projector_schema_parity.rs:18` the 11.) The consumers are code:

| Consumer | Reads |
|---|---|
| CLI `entity`, `report`, `verify` | relation tables **only** — node weights come from `graph_events` |
| CLI `export-graphml` (`--source native`; **no longer the default** — see below) | node + relation tables |
| GraphQL API (dashboard / journalist tier) | same split. **Not** via `EntityLoader` — that loader had no call site for its whole life and was removed 2026-08-23; `schema.rs:227-232` records the reason (it would never batch). The `entity` resolver calls `query_entity_by_name` / `query_entity_by_ch_number` directly |
| `enricher`: `land_flip`, `donation_correlation`, `opencorporates`, `electoral_commission`, PPD district allowlist | `awarded_to`, `located_at`, `controls`, `entity`, `location` |
| CLI `probe` (22-probe battery), `graph-path`, `migrate`, `purge` | node + relation tables |
| CLI `status` | `projection_runs:current` — the marker itself, for the node/edge counts and completion state it reports |
| `enricher`: PPD district allowlist | also reads `projection_runs:current`, refusing to derive an allowlist from an incomplete projection (`land_registry_ppd.rs:530`) |

> **`export-graphml` defaults to `--source events`, not `native` (changed 2026-08-25).**
> `native` reads the projected tables through the SurrealDB SDK, whose flatbuffers codec fails to
> decode typed RecordId fields on large result sets: a 358k-node analysis errors outright with
> `Failed to deserialize flatbuffers payload: Failed to decode fb value`. The `events` path reads
> the `graph_events` WAL over raw HTTP `/sql` and has no such ceiling. `native` remains available
> for small graphs and for exporting exactly what `project` produced — but the default is now the
> path that works at real scale. Related: GRD-05's flatbuffers-ceiling concern.

**The motif detector is the exception** — it does not read the projected tables at all. It
replays `graph_events` into the in-memory CSR snapshot (`replay_from_surreal`, see *CSR Snapshot
& Detection*). That asymmetry matters operationally: on 2026-08-11 an interrupted `project` left
the read model holding 12.6% of the graph, and **detection was completely unaffected** while
every CLI query, adapter and analyst script silently read a fraction. Nothing errored for four
days. See the projection-truncation investigation notes.

## Where the two models diverge

`graph_events` is the source of truth because it is append-only and everything else is derived
from it by a **destructive rebuild**. The projection is therefore:

- **Possibly stale** — it reflects the log as of the last `project` run, nothing since.
- **Possibly partial** — the rebuild wipes before it writes. Check
  `projection_runs:current.finished_at_ms`: null means the last run never completed and the
  tables are a partial rebuild. `scripts/projection_audit.py` leads with this.
- **Lossy by design in places** — `entity_status` collapses `EntityStatus::Dissolved(date)` to
  the bare string `"dissolved"`. ~~and the `entity` table has no dissolution-date column, so that
  date exists *only* in the log~~ — **no longer true since 2026-08-26 (`2b8b9f8`)**: `db.rs:14`
  defines `entity_status_date ON entity` and `projector.rs:877` projects it. More generally, a
  field present in `SCHEMA_DEF` but absent from the projector's `json!` literal vanishes with no
  error (see *Three dropped `contract` fields* in `ARCHITECTURE.md`). ~~A regression test pins the
  projected field set for the `contract` table only — the other nine node tables are unpinned.~~
  **Also fixed:** `crates/ingestor/tests/projector_schema_parity.rs` now pins all **11** node
  tables in **both** directions (schema-defined-but-unmapped, and mapped-but-undefined). The
  narrower `every_schema_defined_contract_field_is_projected` is kept alongside it, since it
  asserts against a fully-populated `ContractData` where the parity test uses serde defaults.

So: derive from the projection for anything that needs joins or traversal, but treat a
disagreement between the two as the projection being wrong until proven otherwise, and never
read an absence from a projected table as evidence of absence in the graph.

## Operational rules that follow

1. **Never read an absence from a projected table as absence in the graph.** "No beneficial
   owners" may mean the `controls` table is empty because `project` has not finished. Check
   `projection_runs:current.finished_at_ms` before believing a zero.
2. **Query `payload.weight_json` and parse client-side.** A SurrealQL path into the payload
   (`payload.weight.some_field`) returns 0 rows, not an error.
3. **When the two models disagree, the projection is wrong until proven otherwise.**
4. **Two tools of record.** `scripts/projection_audit.py` classifies every table
   TRUNCATED / STALE / DROPPED / COMPLETE against the log; the projector's own run summary
   reports `related` / `skipped` with a reason breakdown. They should reconcile exactly — if
   they do not, one of them is measuring the wrong thing, which has happened.
5. **Re-running `project` fixes staleness and truncation. It does not fix a lossy mapping.**
   A field the projector never emits, or a query with the wrong endpoint, survives any number
   of clean runs.

## Worked example: how this failed in practice

On 2026-08-11 a `project` run was SIGTERMed 62,777 rows into a 498,986-row edge stream. It had
already wiped every table, so it left the read model holding **12.6%** of the graph: five
relation tables completely empty, four partial, no error anywhere.

For four days that was investigated as nine separate per-table bugs. It was one interrupted
run. Detection was unaffected throughout — the detector replays the log — while every CLI
query, adapter and analyst script read a fraction of the graph and said nothing. Two
conclusions had already been written into the project's planning notes on the strength of those numbers before
anyone noticed.

What made it invisible, and what changed:

| Property then | Now |
|---|---|
| No record of whether a run finished | `projection_runs:current`, null `finished_at_ms` until the final flush |
| Wipe ran up front, before any rebuild | Split in two, each half deferred until its stream has loaded and been verified |
| Skip-ratio guard divided by rows *iterated*, so unreached rows were invisible | Row count verified against `SELECT count()` before the rebuild starts |

Full analysis: the projection-truncation investigation notes.
