<!-- generated-by: gsd-doc-writer -->
# API Reference

<!-- GSD-MANAGED-DOC: api -->

> **⚠️ Work in progress — this software makes mistakes.** Alerts, scores, verdicts and reports returned by
> these APIs are leads for qualified review, never evidence of wrongdoing. Do not accuse any person or
> organisation on the basis of its output unless a qualified fraud investigator is involved and
> you have taken legal advice first. See [DISCLAIMER.md](../DISCLAIMER.md).

`crates/api` exposes three surfaces from a single Tokio runtime:

| Surface | Address | Source |
|---------|---------|--------|
| HTTP (`/health`, `/metrics`) | `http://127.0.0.1:8080` | `crates/api/src/server.rs`, `crates/api/src/http/health.rs` |
| GraphQL (`/graphql`, `/graphiql`) | `http://127.0.0.1:8080/graphql` | `crates/api/src/graphql/{schema.rs,loaders.rs}` |
| gRPC `urban_integrity.IntegrityService` | `127.0.0.1:8081` | `crates/api/proto/integrity.proto`, `crates/api/src/grpc/integrity.rs` |

Everything on this page was read out of those files. If a route, RPC or GraphQL field is not
listed here, it does not exist — there are exactly **4 HTTP routes**, **4 gRPC RPCs** and
**6 GraphQL query fields**, and no mutations or subscriptions at all
(`Schema<QueryRoot, EmptyMutation, EmptySubscription>`).

> The analyst workbench (`urban-integrity-cli workbench`, `crates/workbench`) runs its own HTTP
> server with a separate `/api/*` surface. That API is documented in `docs/WORKBENCH.md`, not here.

---

## Starting the API

There is one way to start it:

```bash
cargo run -p urban-integrity-cli -- serve
# or, against a named analysis database
cargo run -p urban-integrity-cli -- --analysis <db_name> serve
```

`serve` (`crates/cli/src/main.rs`) loads `config.toml`, replays `graph_events` into the hot
graph, spawns the ingestors and the background enricher, and finally calls
`start_server(handle, ServerConfig::default())`.

### The addresses are compiled in, not configured

`ServerConfig` (`crates/api/src/server.rs`) has exactly two fields and `serve` always passes
`ServerConfig::default()`:

```rust
bind_addr:  "127.0.0.1:8080"          // HTTP + GraphQL
ollama_url: "http://localhost:11434/v1"  // SummarizeAlert only
```

The gRPC address is derived — `bind_addr.port() + 1`, so **8081** — and the gRPC server is
spawned as its own `tonic::transport::Server` on that port rather than merged into the Axum
router, because tonic 0.12 links axum 0.7 internally while this workspace targets axum 0.8.
The comment in `server.rs` records the intent to merge them onto one port once tonic supports
axum 0.8.

**There is no `[api]` section in `config.toml` and no `URBAN__API__*` environment variable.**
Nothing in `crates/core/src/config.rs` describes the listener. To bind a different address or
point `SummarizeAlert` at a different Ollama host you must change `ServerConfig::default()` or
the `serve` arm that constructs it. Binding to loopback is deliberate: this is a local-analyst
deployment, and no authentication layer exists (see *Authentication* below).

### Building it

`crates/api/build.rs` runs `tonic_build` over `proto/integrity.proto` at compile time, so
**`protoc` must be on `PATH`** to build the workspace. CI installs it explicitly before
`cargo test` (`.github/workflows/ci.yml`).

### Reaching each surface

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8080/metrics
curl -s -X POST http://127.0.0.1:8080/graphql \
  -H 'content-type: application/json' \
  -d '{"query":"{ graphStats { nodeCount edgeCount lastSnapshotEvent } }"}'
open http://127.0.0.1:8080/graphiql            # browser IDE
grpcurl -plaintext -d '{}' 127.0.0.1:8081 urban_integrity.IntegrityService/GetGraphStats
```

---

## Authentication

**There is none.** No route, RPC or resolver reads an `Authorization` header, an API key, a
session cookie or a bearer token; there is no auth middleware in the Axum router, no tonic
interceptor, and no auth dependency in `crates/api/Cargo.toml`. The only access control is the
loopback bind address in `ServerConfig::default()`.

The consequence for a deployment: anything that can reach the port can read every alert and
every entity profile in the analysis, including named individuals. Exposing either port beyond
localhost requires an authenticating reverse proxy in front of it — that is an infrastructure
decision this repository does not express.
<!-- VERIFY: any reverse proxy, TLS terminator or network ACL used in a real deployment is outside the repository -->

The one credential the API layer holds is the SurrealDB session inside the shared
`GraphCoordinatorHandle`, configured through `[surreal]` / `URBAN__SURREAL__*` — see
CONFIGURATION.md.

---

## The alert read path — shared by every surface

`alerts list`, `report`, the GraphQL `alerts` field and the gRPC `QueryAlerts` RPC all read
through one function: `query_alerts` (`crates/core/src/persistence/queries.rs`), which
scans and then delegates ordering and filtering to the pure function `select_alerts`
(same file). ARCHITECTURE.md *Alert read path* describes why it was rewritten; this is
what the API surfaces now guarantee.

| Property | Behaviour |
|----------|-----------|
| Ordering | `score DESC`, then `detected_at DESC`, then `id ASC` — **severity, not recency**. The sort is total, so the same query over the same data returns the same sequence every time |
| Filter scope | Every filter is applied over the **complete** `MotifDetected` corpus, loaded by `scan::scan_events_by_kind`. Filters are no longer evaluated inside a recency window, so a sparse or old motif can no longer be invisible at default limits |
| Truncation | `select_alerts` returns `AlertPage { alerts, total_matched }`; `total_matched` is the count **before** the display limit. `AlertPage::truncated()` is true when the limit hid matches |
| `limit == 0` | Means "no display limit" in `select_alerts`. Both API surfaces coerce it away — see the per-surface notes below |

### `total_matched` does not reach either API surface

Both API resolvers read `records.alerts` and discard `records.total_matched`:

- the proto `AlertList` message has a single field, `repeated Alert alerts` — there is no count
  field on the wire (`crates/api/proto/integrity.proto`);
- the GraphQL `alerts` field returns `[AlertObject!]!` — a bare list, no page wrapper
  (`crates/api/src/graphql/schema.rs`).

Only the CLI prints the total, and only when truncation actually occurred:

```
Note: showing 100 of 1947 matching alerts, highest score first — use --limit N for more, or --limit 0 for all.
```

**An API client therefore cannot distinguish "that is all of them" from "that is the first
`limit` of them."** Until a count field is added, raise the limit (gRPC up to 1000) and compare
the returned length against it.

### Why filtering happens in Rust rather than SurrealQL

The alert payload lives in `payload.result_json`, which is a JSON **string** rather than a
structured column, and SurrealDB cannot index into it. `parse_alert_row` (`queries.rs`)
deserialises that string to recover `motif_type`, `risk_score`, `score_breakdown.ward_code`,
`withdrawn_at` and `withdrawn_reason` — none of which exist as queryable fields.

So every `alerts` / `QueryAlerts` call performs a **full paginated scan** of
`graph_events WHERE kind = 'MotifDetected'` (`SCAN_PAGE_SIZE = 20_000` rows per round trip,
safety cap `SCAN_MAX_ROWS = 2_000_000` which logs a truncation WARN if ever reached), then
filters and sorts in Rust. Cost is bounded by the **alert** corpus rather than the graph — 1,947
rows on the live Anglesey analysis, measured 2026-08-18 — but it is a full scan on every request,
which is the relevant constraint when calling these surfaces in a loop.
<!-- VERIFY: 1,947 live alert rows — a runtime measurement against a populated database, not derivable from repository contents -->

### `motif_type` values, and the CLI-only shorthands

Filtering compares the requested `motif_type` case-insensitively against the whole stored
string (`a.motif_type.eq_ignore_ascii_case(m)`). The values that appear in the store are the
eight `MotifType` variants (`crates/detector/src/alert/result.rs`, `MotifType::ALL`) — three
serialised under their variant names —

`PhoenixLoop` · `AddressCluster` · `NepotismBridge`

— and five serialised as snake_case strings via `serde(rename)`, which the post-adapter detectors
in `crates/enricher` emit:

`property_accumulation_anomaly` · `disqualified_director_evasion` · `donation_contract_correlation` · `offshore_shadow` · `charity_commercial_crossover`

**Pass the stored value.** The short forms `phoenix`, `cluster`, `nepotism`, `property`, `evasion`,
`offshore`, `charity` and `donation` are a CLI-only convenience: `normalize_motif_type`
(`crates/cli/src/commands/alerts.rs`) maps them before
querying, and it is used by `alerts list` only. Neither API surface performs that mapping, and
neither does `report --motif` (`crates/cli/src/commands/report.rs` passes `args.motif` through raw
when building its alert filter). An unmatched value returns an empty list rather than an error.

---

## Alert Withdrawal — a Publishing-Surface Guarantee

An alert names a real person or a real company. A finding that a later data correction has proved
false is therefore not a neutral row: publishing it is an accusation the engine no longer stands
behind. **The alert-listing surfaces are fail-closed against retracted alerts** — a withdrawn
alert never leaves the GraphQL `alerts` field or the gRPC `QueryAlerts` RPC, and no client can
ask for one. Since 2026-08-20 that guarantee covers **every** read path served by `crates/api`, including the
by-id ones and the entity profile — see *Where the filter runs* below.

### How a withdrawal is stored

`MotifResult` (`crates/detector/src/alert/result.rs`) carries two optional fields:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub withdrawn_at: Option<DateTime<Utc>>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub withdrawn_reason: Option<String>,
```

Both attributes are load-bearing:

- `serde(default)` — every alert already persisted predates these fields and has neither key.
  Without the default, those rows would fail to parse and vanish, which is a worse failure than the
  one the fields fix.
- `skip_serializing_if` — a live alert's `result_json` stays byte-identical to what earlier
  versions wrote, so nothing downstream has to learn a new shape to read an unwithdrawn alert.

The withdrawal is written **inside `result_json`**, not beside it, so it travels with the alert
through every existing reader and `parse_alert_row` remains the single place an alert is decoded.
`AlertRecord` (`crates/core/src/persistence/queries.rs`) mirrors the two fields as
`Option<String>`.

### The filter

`crates/core/src/persistence/queries.rs`, `alert_passes_withdrawn_filter`:

```rust
pub fn alert_passes_withdrawn_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter.include_withdrawn || alert.withdrawn_at.is_none()
}
```

An alert survives only if the caller explicitly asked for withdrawn alerts, or the alert was never
withdrawn. `AlertFilter::include_withdrawn` defaults to `false`, so a surface that forgets to think
about withdrawal gets the safe behaviour rather than the permissive one.

### Who can opt in

| Surface | `include_withdrawn` | Can a client change it? |
|---------|---------------------|-------------------------|
| GraphQL `alerts` (`crates/api/src/graphql/schema.rs`) | hard-coded `false` | **No** — it is not a query argument |
| GraphQL `alert(id:)` (`crates/api/src/graphql/loaders.rs`) | filtered in `AlertLoader` | **No** — a withdrawn alert resolves to `null` |
| GraphQL `entity { motifAlerts }` (`crates/core/src/persistence/queries.rs`) | filtered in `assemble_entity_profile`; the list is uncapped | **No** |
| gRPC `QueryAlerts` (`crates/api/src/grpc/integrity.rs`) | hard-coded `false` | **No** — there is no such field in the `AlertFilter` message |
| gRPC `GetAlert` (`crates/api/src/grpc/integrity.rs`) | filtered in the handler | **No** — a withdrawn alert returns `NOT_FOUND` |
| CLI `report` | hard-coded `false` | No |
| CLI `alerts show <ID>` | not filtered | n/a — operator-local by-id read |
| CLI `alerts list` | `--include-withdrawn` (default `false`) | **Yes** — operator-local audit path |
| Workbench `GET /api/alerts` (`crates/workbench/src/alerts/mod.rs`) | `include_withdrawn` query parameter | **Yes** — see `docs/WORKBENCH.md` |

The proto omission is deliberate. Adding `include_withdrawn` to the `AlertFilter` message would
make the guarantee negotiable by the caller, and the caller is the party the guarantee protects
third parties from. The CLI keeps the opt-in because a retracted false positive is evidence about
detector quality and an operator auditing the detector needs to see it.

### Where the filter runs

`alert_passes_withdrawn_filter` only governs the two list paths (`query_alerts` → `select_alerts`).
The three by-id and by-entity paths each filter at their own boundary, because the shared core
fetches they sit on — `get_alert_by_id`, `get_alerts_by_ids` — are deliberately neutral so the
operator's CLI can still read a withdrawn alert:

| Path | Where it filters | Result of asking for a withdrawn alert |
|------|------------------|----------------------------------------|
| gRPC `GetAlert` | `crates/api/src/grpc/integrity.rs` — matches `Ok(Some(r)) if r.withdrawn_at.is_some()` | `NOT_FOUND`, the same status as an unknown id |
| GraphQL `alert(id:)` | `AlertLoader::load` (`crates/api/src/graphql/loaders.rs`) drops the row before mapping to `AlertObject` | `null` |
| GraphQL `entity { motifAlerts }` | `crates/core/src/persistence/queries.rs` (`assemble_entity_profile`); the list is not capped | the alert is absent from the list |

Two design points worth carrying into any new surface:

- **`NOT_FOUND`, not a distinct status.** Neither `AlertObject` nor the proto `Alert` message has
  a withdrawal field, so returning the row would have published a disowned accusation as a live
  finding, with the retraction visible only to a client that happened to parse `result_json`.
- **`motifAlerts` is uncapped.** It returns every live alert naming the entity, sorted by score
  descending then id, so a withdrawn alert cannot push a live one out of view.

Outside `crates/api`, the readers that still surface a withdrawn alert are the operator-local
`alerts show <ID>` (unfiltered by-id read) and `alerts list --include-withdrawn`, plus the
workbench server's `GET /api/alerts`, which accepts an `include_withdrawn` query parameter (see
`docs/WORKBENCH.md`).

### A withdrawal is a retraction, not a permanent suppression

Detection upserts what it matches. If the motif starts matching again, the upsert overwrites the
stored result and the withdrawal is cleared. That is correct — it is a live finding once more —
but it means a withdrawal cannot be used as a suppress-list entry, and re-running detection after
correcting the underlying data may resurrect an alert an operator previously retracted.

### Operator flow

```bash
# Retract an alert a data correction has invalidated
urban-integrity-cli alerts withdraw <alert-id> --reason "rests on a name-only officer merge; split 2026-08-11"

# Read it back for audit (the only surface that will show it)
urban-integrity-cli alerts list --include-withdrawn
```

Withdrawal **marks**, never deletes: the row stays as evidence about detector quality.
`alerts withdraw` refuses to re-withdraw an already-withdrawn alert rather than silently
overwriting the original reason and timestamp.

---

## HTTP Endpoints

The Axum router in `start_server` declares exactly four routes. Anything else is a 404, and
`GET /graphql` is a 405 (only `POST` is routed).

| Method | Path | Handler | Description |
|--------|------|---------|-------------|
| `GET` | `/health` | `health_ext` | Engine status plus live graph counts |
| `GET` | `/metrics` | `metrics_ext` | Graph statistics for monitoring |
| `POST` | `/graphql` | `graphql_handler` | Executes a GraphQL request against `AppSchema` |
| `GET` | `/graphiql` | `graphiql_handler` | GraphiQL IDE HTML, pointed at `/graphql` |

A `TraceLayer` is applied to all four; the coordinator handle and the schema are supplied as
Axum `Extension`s so both can sit on the merged router. `/health` and `/metrics` read only the
pre-baked `Arc<CsrSnapshot>` via `ArcSwap::load()` — no database round trip.

> `crates/api/src/http/health.rs` also defines `health_handler` / `metrics_handler`, which take
> the handle via `State` instead of `Extension`. They are not the ones wired into the router;
> the routed handlers are `health_ext` / `metrics_ext` in `server.rs`. Both produce the same
> `HealthResponse` / `MetricsResponse` bodies.

### `GET /health`

`version` is `CARGO_PKG_VERSION` of the `urban-integrity-api` crate — `0.1.0` at the version in
`crates/api/Cargo.toml`. `status` is the literal `"ok"`; there is no failing branch, so this is a
liveness check, not a readiness check — it does not test the database connection.

```json
{
  "status": "ok",
  "version": "0.1.0",
  "node_count": 42731,
  "edge_count": 118452
}
```

### `GET /metrics`

JSON, not Prometheus exposition format.

```json
{
  "node_count": 42731,
  "edge_count": 118452,
  "last_snapshot_event": 98765
}
```

`last_snapshot_event` is `CsrSnapshot::baked_at_event` — the coordinator event count at which the
current snapshot was baked. It is `0` before the first bake.

---

## gRPC API

Defined in `crates/api/proto/integrity.proto`, implemented by `IntegrityServiceImpl`
(`crates/api/src/grpc/integrity.rs`). Package `urban_integrity`, service `IntegrityService`,
served on `127.0.0.1:8081` in plaintext (no TLS, no interceptors).

### Service: `IntegrityService`

| RPC | Request | Response | Description |
|-----|---------|----------|-------------|
| `QueryAlerts` | `AlertFilter` | `AlertList` | Alerts by score / ward / motif, ordered by score descending over the whole corpus. `limit > 1000` → `INVALID_ARGUMENT`. Withdrawn alerts are never returned and cannot be requested |
| `GetAlert` | `AlertId` | `Alert` | One alert by `event_id`. `NOT_FOUND` when absent — and equally when the alert has been withdrawn, so a retraction is indistinguishable from an absence on the wire |
| `SummarizeAlert` | `AlertId` | `AlertSummary` | Natural-language summary via a local Ollama model. `UNAVAILABLE` when Ollama is not running; no other RPC is affected |
| `GetGraphStats` | `Empty` | `GraphStats` | Node count, edge count and last snapshot event from the current CSR snapshot |

Failure modes shared by the two database RPCs: a SurrealDB error is logged and returned as
`INTERNAL` with the error text.

### Message Types

#### `AlertFilter`

```protobuf
message AlertFilter {
  optional float min_score = 1;    // omitted → 0.0, i.e. no score floor
  optional string ward_code = 2;   // exact match, e.g. "X05999001"
  optional string motif_type = 3;  // stored variant name, case-insensitive
  optional string from_date = 4;   // ACCEPTED AND IGNORED — see below
  optional string to_date = 5;     // ACCEPTED AND IGNORED — see below
  int32 limit = 6;                 // must be <= 1000; 0 or negative is coerced to 1
}
```

Three details that are only visible in the handler, not the proto:

- **`from_date` and `to_date` are silently dropped.** `query_alerts` (the gRPC one, in
  `integrity.rs`) builds its internal filter with `from_date: None, to_date: None` regardless of
  what the client sent. The core filter does support date bounds — the CLI uses them — but the
  gRPC surface does not forward them. Sending them narrows nothing and produces no error.
- **`limit` has no "all" value.** The handler rejects `> 1000` with `INVALID_ARGUMENT`, then
  applies `filter.limit.max(1)`, so `0` (the proto default for an unset `int32`) becomes **1**,
  not unlimited. A client that omits `limit` entirely gets a single alert back. Always set it.
- **There is deliberately no `include_withdrawn` field.** The service hard-codes it to `false`.

#### `Alert`

```protobuf
message Alert {
  string id = 1;              // graph_events.event_id
  string motif_type = 2;      // e.g. "PhoenixLoop", "property_accumulation_anomaly"
  float score = 3;            // P_c score 0.0–1.0 (f64 in the store, narrowed to f32 here)
  string result_json = 4;     // the full MotifResult, serialised
  uint64 detected_at = 5;     // Unix epoch milliseconds; 0 if the stored timestamp fails to parse
  optional string ward_code = 6;  // from result_json.score_breakdown.ward_code
}
```

> `result_json` is the full `MotifResult` (`crates/detector/src/alert/result.rs`): `motif_type`,
> `unique_hash`, `participants`, `risk_score`, `evidence_snapshot`, `detected_at_event`, `version`,
> `last_seen_at` and `score_breakdown`. On a withdrawn alert it additionally carries
> `withdrawn_at` and `withdrawn_reason`; on a live alert both keys are absent entirely
> (`skip_serializing_if`). Since both `QueryAlerts` and `GetAlert` filter withdrawn alerts out,
> **those two keys can no longer reach a gRPC client at all** — they are visible only on the
> operator's CLI.
>
> `score_breakdown` is `ScoreBreakdown`: `s_phoenix`, `s_proximity`, `s_clustering`, `p_base`,
> `m_env`, `p_final`, `imd_rank` (nullable) and `ward_code` (nullable). **`m_env = 1.0` with a
> null `imd_rank` is not a finding that the area is un-deprived** — it is what every alert carries
> when no deprivation index is loaded, or when the loaded index does not cover the alert's ward.
> `data/*` is gitignored, so a fresh clone has no index and every alert reads that way
> (`crates/detector/src/lib.rs`, `crates/detector/src/scoring/formula.rs`).

#### `AlertList`

```protobuf
message AlertList {
  repeated Alert alerts = 1;
}
```

No total, no cursor, no page token. See *`total_matched` does not reach either API surface*.

#### `GraphStats`

```protobuf
message GraphStats {
  uint64 node_count = 1;           // nodes in the current CSR snapshot
  uint64 edge_count = 2;           // edges in the current CSR snapshot
  uint64 last_snapshot_event = 3;  // event count when the snapshot was baked
}
```

#### `AlertSummary`

```protobuf
message AlertSummary {
  string text = 1;    // natural-language summary, or "No summary available"
  string model = 2;   // always "gemma3" — hard-coded in the handler
}
```

### `SummarizeAlert` and Ollama

`SummarizeAlert` POSTs to `{ollama_url}/chat/completions` — by default
`http://localhost:11434/v1/chat/completions`, the Ollama OpenAI-compatible endpoint — with the
`gemma3` model, a fixed "procurement fraud analyst" system prompt, `stream: false` and a
30-second timeout. The alert ID is interpolated into the prompt; the alert body is not fetched
from the database first.

**Ollama is optional and its absence is not an error condition for the engine.** If it is not
running, the request fails, a `WARN` is logged, and this one RPC returns `UNAVAILABLE` carrying
the transport error. Nothing else is affected: the server keeps serving, and every other RPC,
GraphQL query and HTTP endpoint continues to work. There is no startup check for Ollama and no
code path that treats its absence as fatal.

The CLI `--summarize` flag on `report` and `verify` is a **separate** code path
(`crates/cli/src/ollama.rs`) that targets the `gemma4` model at the same host, and degrades the
same way — a `WARN` and output without the narrative section. See CONFIGURATION.md
*Local LLM (Ollama)*.

### gRPC Examples (grpcurl)

```bash
# Highest-scoring address-cluster alerts in ward X05999001
# motif_type is the stored variant name — not the CLI's short `cluster` form
grpcurl -plaintext -d '{
  "min_score": 0.7,
  "ward_code": "X05999001",
  "motif_type": "AddressCluster",
  "limit": 20
}' 127.0.0.1:8081 urban_integrity.IntegrityService/QueryAlerts

# The top 1000 alerts across the whole corpus, highest score first.
# `limit` is mandatory in practice: omitting it returns exactly one alert.
grpcurl -plaintext -d '{"limit": 1000}' \
  127.0.0.1:8081 urban_integrity.IntegrityService/QueryAlerts

# Graph statistics
grpcurl -plaintext -d '{}' 127.0.0.1:8081 urban_integrity.IntegrityService/GetGraphStats

# LLM summary of an alert (UNAVAILABLE if Ollama is not running — nothing else breaks)
grpcurl -plaintext -d '{"id": "alert-uuid-here"}' \
  127.0.0.1:8081 urban_integrity.IntegrityService/SummarizeAlert
```

---

## GraphQL API

Endpoint: `http://127.0.0.1:8080/graphql` (POST only)
Interactive IDE: `http://127.0.0.1:8080/graphiql` (browser)

Built with `async-graphql` 7 + `async-graphql-axum` over Axum 0.8.
`AppSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>` — **queries only; there are no
mutations and no subscriptions.**

**Security limits** applied in `build_schema`:

- Max query depth: **10** (`limit_depth`)
- Max query complexity: **1000** (`limit_complexity`)

**DataLoader.** Exactly one `async_graphql::dataloader::DataLoader` is registered, and only when
the coordinator handle carries a live database — in test contexts (`db = None`) the resolvers
degrade to `[]` / `null` rather than panicking:

| Loader | Used by | Implementation |
|--------|---------|----------------|
| `AlertLoader` | `alert(id:)` — every selection in one request collapses into one batch | a single `get_alerts_by_ids` query for the whole batch, then withdrawn rows are dropped |

`alert(id:)` is the only field that goes through a DataLoader, and the only one that would
otherwise N+1. Every other resolver calls its query function directly: `entity` uses
`query_entity_by_name` / `query_entity_by_ch_number` (D-09), and `alerts`, `graphStats`,
`graphPath` and `enrichmentStatus` likewise.

An `EntityLoader` was also registered here from v2.0 until **2026-08-23**, when it was removed as
dead wiring — it had no call site for its entire life. Nothing was lost: `EntityProfile` is a
`SimpleObject` materialised by a single query, and the schema exposes no per-parent entity
resolver, so there was no N+1 for it to collapse. Adding a loader back would only be justified
alongside a lazy sub-resolver that actually fetches entities per parent.

### Queries

#### `alerts`

List flagged motif alerts, highest score first.

```graphql
alerts(
  minScore: Float = 0.5
  wardCode: String
  motifType: String
  limit: Int = 50
): [AlertObject!]!
```

- `minScore` defaults to **0.5**, matching the CLI's `--min-score` default. Pass `0` for no floor.
- `limit` defaults to **50** and is applied as `limit.max(1)`, so `0` and negative values become
  **1** — there is no "return everything" value here, unlike the CLI's `--limit 0`. There is no
  upper cap on the GraphQL side (the 1000 ceiling is gRPC-only), but a large limit means a larger
  in-memory sort over the same full scan.
- There are **no** `fromDate` / `toDate` arguments; the resolver passes `None` for both.
- There is **no** `includeWithdrawn` argument, by design.
- Returns `[]` (not an error) when the handle has no database.

**`AlertObject` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | `String!` | `graph_events.event_id` |
| `motifType` | `String!` | Stored motif string, e.g. `"AddressCluster"` |
| `score` | `Float!` | P_c risk score 0.0–1.0 |
| `resultJson` | `String!` | Full `MotifResult` as a JSON string |
| `detectedAt` | `String!` | RFC 3339 timestamp derived from the event timestamp |
| `wardCode` | `String` | `result_json.score_breakdown.ward_code`, when the alert was ward-attributed |

`AlertObject` deliberately has **no** `withdrawnAt` field — withdrawn alerts never reach this
resolver. (`AlertRecord`, returned by `entity { motifAlerts }`, does expose it. See
*Alert Withdrawal*.)

#### `alert`

One alert by ID, resolved through `AlertLoader` so that several `alert(id:)` selections in the
same request collapse into a single `get_alerts_by_ids` query.

```graphql
alert(id: String!): AlertObject
```

Resolves to `null` when the ID is unknown, when the ID fails the `is_valid_event_id` sanitiser
inside `get_alerts_by_ids`, when no database handle (and therefore no DataLoader) is present, or
when the alert has been **withdrawn** — `AlertLoader` drops retracted rows before they become
`AlertObject`s, so a retraction is indistinguishable from an unknown id here.

#### `graphStats`

Live graph engine statistics read directly from the current CSR snapshot — no database access.

```graphql
graphStats: GraphStatsObject!
```

| Field | Type | Description |
|-------|------|-------------|
| `nodeCount` | `Int!` | Nodes in the current snapshot |
| `edgeCount` | `Int!` | Edges in the current snapshot |
| `lastSnapshotEvent` | `Int!` | Event count at which the snapshot was baked |

#### `entity`

Full entity profile by name or Companies House number. Returns `null` when nothing matches or
when no database handle is present.

```graphql
entity(
  name: String
  chNumber: String
): EntityProfile
```

At least one argument must be supplied; with neither, the resolver returns `null` without
querying. **When both are supplied, `name` wins** — `chNumber` is consulted only when `name` is
absent.

Name lookup is a case-insensitive **substring** scan over Entity node events, truncated to 10
matches, of which the resolver returns the **first**. When fewer than 10 Entity rows match,
`query_entity_by_name` also scans `OffshoreEntity` nodes (OCOD / ROE overseas owners, de-duplicated
by id) into the same 10-result budget, so an overseas owner can be the match that is returned. It is not a phonetic or similarity match
(in-app fuzzy matching was removed from the engine in 2026-06), and unlike the CLI it does **not**
raise a disambiguation error — an ambiguous name silently yields one arbitrary match. Use
`chNumber`, which is an exact lookup, when the answer must be unambiguous.

**`EntityProfile` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | `String!` | HIN entity ID (e.g. `"GB-COH:12345678"`) |
| `name` | `String!` | Display name |
| `chNumber` | `String` | Companies House registration number |
| `status` | `String!` | `"active"` \| `"dissolved:YYYY-MM-DD"` \| `"in_liquidation:YYYY-MM-DD"` — rendered by a `ComplexObject` resolver over the `EntityStatus` enum |
| `incorporationDate` | `String` | ISO date |
| `addresses` | `[AddressRow!]!` | Registered and operational addresses |
| `directors` | `[DirectorRow!]!` | Known directors |
| `contracts` | `[ContractRow!]!` | Contract award records (planning applications are in `planningApplications`) |
| `planningApplications` | `[PlanningApplicationRow!]!` | Planning application records, split out of `contracts` |
| `motifAlerts` | `[AlertRecord!]!` | Alerts whose `result_json` mentions this entity — withdrawn alerts are dropped; uncapped, sorted by score descending then id |
| `lpaPresence` | `[LpaPresence!]!` | LPA codes with contract/application counts |
| `beneficialOwners` | `[BeneficialOwnerRow!]!` | PSC data from `identity_links` (`source='ch_psc'`, auto-linked or confirmed) |
| `disqualifications` | `[DisqualificationRow!]!` | CDDA 1986 disqualification records |
| `osintLinks` | `[OsintLink!]!` | Confirmed / auto-linked cross-registry identity links (max 100; `pending_review` never surfaced) |
| `sanctionsStatus` | `SanctionsStatus` | OpenSanctions match status via the external yente service |
| `osintAlerts` | `[OsintAlertRow!]!` | OSINT-sourced motif alerts (uncapped) |
| `offshoreLinks` | `[LinkedRecordRow!]!` | Offshore link records |
| `trusteeOf` | `[LinkedRecordRow!]!` | Trustee-of records |
| `ownsProperty` | `[PropertyRow!]!` | Property ownership records |
| `donatesTo` | `[LinkedRecordRow!]!` | Political donation records |

> `relatedEntities` and `subCandidateLinks` are `#[graphql(skip)]` and are **not** in the schema.
> Use the CLI `entity --output json` for related entities and `entity --deep` for sub-candidate
> links.

`AlertRecord` — the type behind `motifAlerts` — carries the same six fields as `AlertObject` plus
`withdrawnAt: String` and `withdrawnReason: String`, and also `participants: [String!]!`,
`firstSeenRunId: String` and `lastSeenRunId: String`. Those two are the only withdrawal fields in
the GraphQL schema, and since the resolver now filters retracted alerts out they are **always
`null`** in practice; they remain because `AlertRecord` derives `SimpleObject` over every field and
the CLI reads the same type.

#### `graphPath`

Shortest path between two entities via BFS over `graph_events`, traversed undirected and capped
at `MAX_HOPS` = **6** (`crates/core/src/persistence/traversal.rs:18`).

```graphql
graphPath(
  from: String!
  to: String!
): [PathStep!]!
```

Both arguments go through `resolve_entity`, which accepts a slug, a CH number or a name
substring. Unlike `entity`, ambiguity here **is** an error: an input resolving to more than one
entity produces a GraphQL error listing the candidate ids (`Ambiguous: [...] — use a slug`), and
an input resolving to none produces `Entity not found: '...'`. Identical endpoints, no path
found, or no database handle all yield `[]`.

| `PathStep` field | Type | Description |
|------------------|------|-------------|
| `id` | `String!` | HIN entity ID at this step (`node_id` internally) |
| `label` | `String!` | Display name at this step (`node_label` internally) |
| `edgeType` | `String` | Edge type connecting this step to the next; `null` on the final step |

#### `enrichmentStatus`

Identity-link enrichment statistics for a company number. Returns `null` when no identity links
exist for it, when the input fails CH-number validation, or when no database handle is present —
never an error for bad input.

```graphql
enrichmentStatus(
  companyNumber: String!
): EnrichmentStatusResult
```

| Field | Type | Description |
|-------|------|-------------|
| `companyNumber` | `String!` | The Companies House number queried |
| `totalLinks` | `Int!` | Total `identity_links` rows for this company |
| `autoLinked` | `Int!` | Rows auto-linked at confidence ≥ 0.85 (not analyst-reviewed) |
| `pendingReview` | `Int!` | Rows awaiting an analyst decision |
| `confirmed` | `Int!` | Analyst-approved rows |
| `lastEnriched` | `String` | ISO 8601 timestamp of the most recent enrichment run |

### Example Queries

```graphql
# Highest-scoring alerts in a ward, filtered by motif
# motifType is the stored variant name: PhoenixLoop | AddressCluster | NepotismBridge
# (or one of the enricher's snake_case motif strings)
query {
  alerts(minScore: 0.7, wardCode: "X05999001", motifType: "AddressCluster", limit: 200) {
    id
    motifType
    score
    detectedAt
    wardCode
  }
}

# Everything above the default floor, most severe first.
# There is no "unlimited" value — set an explicit limit and compare the
# returned length against it to detect truncation.
query {
  alerts(minScore: 0.0, limit: 500) { id motifType score }
}

# Graph engine health check — no database round trip
query {
  graphStats {
    nodeCount
    edgeCount
    lastSnapshotEvent
  }
}

# Fetch multiple alerts by ID — AlertLoader batches these into one query
query {
  a1: alert(id: "uuid-1") { id motifType score }
  a2: alert(id: "uuid-2") { id motifType score }
}

# Full entity profile by name (first substring match wins — no disambiguation)
query {
  entity(name: "Apex Build Ltd") {
    id
    name
    chNumber
    status
    directors { name role appointed }
    # withdrawnAt / withdrawnReason are always null here — retracted alerts are filtered out
    motifAlerts { id motifType score withdrawnAt withdrawnReason }
    beneficialOwners { name naturesOfControl }
  }
}

# Entity lookup by Companies House number — the unambiguous path
query {
  entity(chNumber: "12345678") {
    id
    name
    status
    disqualifications { officerName disqualifiedFrom disqualifiedUntil }
    sanctionsStatus { onList matchName matchScore }
  }
}

# Shortest path between two entities (undirected BFS, max 6 hops)
query {
  graphPath(from: "Apex Build Ltd", to: "director:john-smith") {
    id
    label
    edgeType
  }
}

# Enrichment pipeline status for a company
query {
  enrichmentStatus(companyNumber: "12345678") {
    companyNumber
    totalLinks
    autoLinked
    pendingReview
    confirmed
    lastEnriched
  }
}
```

---
## CLI

The `urban-integrity-cli` binary provides the primary human-facing interface. (Its clap `name` is
`urban-integrity`, but `--help` prints the invoked binary name, `urban-integrity-cli`, which the usage
blocks below use. Run it with `cargo run -p urban-integrity-cli -- <command>` if it is not on your
`PATH`.)

```
USAGE:
    urban-integrity [--analysis <NAME>] <COMMAND>

GLOBAL OPTIONS:
    --analysis <NAME>    Operate on a named analysis database (registry db_name,
                         e.g. an_example_00000000) [default: main]. Bridged to
                         the URBAN__ANALYSIS env var so every command's config
                         load resolves the same analysis.

COMMANDS:
    report           Generate an integrity report for a ward or time range
    alerts           List, inspect and withdraw flagged motif alerts
    anomalies        List and adjudicate data anomalies — inputs the pipeline
                     refused to guess at (deliberately NOT alerts)
    status           Show graph engine status and statistics
    entity           Query entity profile by name or Companies House number
    graph-path       Find the shortest path between two entities in the HIN graph
    serve            Start the graph engine (API server + ingestors)
    ingest-planning  Scrape planning portal data on demand
    backfill         Backfill enrichment data (CH numbers, ward codes) into the live graph
    project          Project graph_events WAL into native SurrealDB tables for visual exploration
    export-graphml   Export the graph as a spec-compliant GraphML file (reads the WAL by default)
    verify           Run a pre-signature risk audit for a Companies House entity
    enrich           Run OSINT enrichment against configured sources
    links            Review and manage OSINT identity resolution links
    flips            Review pre-permission land-flip candidates
    migrate          Apply IN/OUT schema constraints to all 12 relation tables and re-project
    purge-orphans    Purge orphan Property nodes + identity_links left by the pre-fix CCOD ingest
    purge-ocod-owners  Retire superseded GB-OCOD: owner nodes + ownership edges from graph_events
    analysis         Provision and inspect analyses (create, list, remove) — the analysis lifecycle
    fixture          Seed fixture graphs into a --fixture analysis (never a live corpus)
    probe            Run the Tier-1 forensic probe battery against graph_events
    probe-oracle     Compare every probe's declared population against an independent
                     graph_events count, and state why the rest cannot be checked
    workbench        Boot a read-only replay snapshot and serve the analyst workbench UI
```

That is the complete set — the `Commands` enum in `crates/cli/src/main.rs` declares exactly
**23 subcommands**.

> `probe`'s clap doc comment still reads "Tier-1 forensic probe battery ... (PRB-02, Tier-1
> slice)". That wording is stale twice over: `build_battery`
> (`crates/probe/src/run.rs`) registers **22** probes — 7 that report
> `tier() == "Tier-1"` and 15 that report `"Tier-2"` — and a default run is gated rather than a
> straight sweep of them. The help text has not caught up with the registry.

### `report` — Generate Integrity Report

```
urban-integrity-cli report [OPTIONS]

OPTIONS:
    --ward <WARD>              Ward code (e.g. X05999001)
    --min-score <SCORE>        Minimum P_c score [default: 0.5]
    --motif <MOTIF>            Filter by motif. Pass the stored variant name:
                               PhoenixLoop | AddressCluster | NepotismBridge
    --from <DATE>              Start date (YYYY-MM-DD)
    --to <DATE>                End date (YYYY-MM-DD)
    --format <FORMAT>          Output format: table | json | md | pdf
                               (--output is an accepted alias)
    --output-file <PATH>       Write report to file (md and pdf formats only)
    --entity <NAME_OR_SLUG>    Entity-scoped report across all wards/LPAs
    --scope <SCOPE>            Cross-LPA correlated scope for --entity reports
                               (e.g. all-lpa)
    --summarize                Prepend LLM narrative via Ollama (graceful WARN if unavailable)
    --limit <N>                Maximum alerts to include; 0 = all [default: 100]
```

### `alerts` — List Flagged Alerts

```
urban-integrity-cli alerts <SUBCOMMAND>

SUBCOMMANDS:
    list             List flagged motifs with optional filters
    show <ID>        Show full detail for a specific alert
    withdraw <ID>    Retract an alert a data correction has invalidated
```

### `status` — Engine Statistics

```
urban-integrity-cli status [OPTIONS]

OPTIONS:
    --output <FORMAT>    table | json
```

Prints the database name, the `graph_events` count, and the projection state from
`projection_runs:current` (`Complete`, `Unfinished` or `NeverRun`); for a complete projection it
also prints node, edge and skipped-edge counts.

### `entity` — Entity Profile

```
urban-integrity-cli entity [OPTIONS] [QUERY]

ARGS:
    [QUERY]    Company name (substring match) or slug (exact ID lookup)

OPTIONS:
    --ch-number <CH>    Companies House registration number
    --output <FORMAT>   table | json
    --deep              Include sub-candidate OSINT links (0.50–0.69, investigative only)
```

### `serve` — Start Engine

```
urban-integrity-cli serve
```

Starts all ingestors, the background enricher, the HTTP/GraphQL server on
`127.0.0.1:8080` and the gRPC server on `127.0.0.1:8081`. Both addresses come from
`ServerConfig::default()` and are not configurable — see *Starting the API* above.

### `ingest-planning` — On-demand Planning Portal Scrape

```
urban-integrity-cli ingest-planning [OPTIONS]

OPTIONS:
    --ward <WARD_CODE>    Scrape a single configured ward
    --all                 Scrape all portals in [[ingest.planning_portals]] config
```

---

## CLI Reference (Extended)

This section documents subcommands and flags in full detail.

### Output format auto-detection

All commands that accept `--output` or `--format` auto-select the format when the flag is omitted:
- **Terminal (TTY):** `table` — coloured, human-readable
- **Pipe / redirect:** `json` — machine-readable, pretty-printed

### Full command list

```
COMMANDS:
    report           Generate an integrity report for a ward or time range
    alerts           List, inspect and withdraw flagged motif alerts
      list             List flagged motifs with optional filters
      show <ID>        Show full detail for a specific alert
      withdraw <ID>    Retract an alert invalidated by a data correction
    anomalies        List and adjudicate data anomalies — inputs the pipeline
                     refused to guess at
      list             List open anomalies, highest severity first
      show <ID>        Show the full evidence for one anomaly
      review <ID>      Mark an anomaly adjudicated (--dismiss to close it out)
    status           Show graph engine status and statistics
    entity           Query entity profile by name or Companies House number
    graph-path       Find the shortest path between two entities in the HIN graph
    serve            Start the graph engine (API server + ingestors)
    ingest-planning  Scrape planning portal data on demand
    backfill         Backfill enrichment data (CH numbers, ward codes) into the live graph
      ch               Link planning applicants to Companies House numbers
      ward             Resolve ward codes for contracts via postcodes.io
      planning         Re-scrape unattributed planning wards and stamp
                       applicant provenance across the planning corpus
      cessation        Fetch real date_of_cessation from Companies House so
                       Phoenix's window anchors where the company failed
      officers         Fetch officers for CH entities that have none, keyed on
                       CH identity evidence rather than on the name alone
      psc-links        Re-run the PSC↔officer identity join in-graph
                       (no Companies House API calls)
    project          Project graph_events WAL into native SurrealDB tables
    export-graphml   Export the graph as a spec-compliant GraphML file
                     (--source events|native, --node-type, --edge-type, --drop-isolated)
    verify           Run a pre-signature risk audit for a Companies House entity
    enrich           Run OSINT enrichment against configured sources
    links            Review and manage OSINT identity resolution links
      list             List pending identity links by confidence
      show <ID>        Show evidence detail for a specific link
      confirm <ID>     Confirm a link and write an EdgeConfirmed WAL event
      reject <ID>      Reject a link and suppress re-proposal
    flips            Review pre-permission land-flip candidates (review queue only)
      list             List pending land-flip candidates (newest first)
      show <ID>        Show all three gate-evidence sections for one candidate
      confirm <ID>     Confirm a candidate (optional --note; does NOT touch the hot graph)
      dismiss <ID>     Dismiss a candidate (optional --note; suppresses re-proposal)
    migrate          Apply IN/OUT schema constraints to all 12 relation tables and re-project
    purge-orphans    Purge orphan Property nodes + identity_links left by the pre-fix CCOD ingest
    purge-ocod-owners  Retire superseded GB-OCOD: owner nodes + ownership edges from
                     graph_events (dry-run by default; --apply requires typing back a
                     run-derived confirmation digest)
    analysis         Provision and inspect analyses — the analysis lifecycle
      create           Provision a new analysis (optionally cloning a baseline with --from)
      list             List provisioned analyses
      remove <DB>      Remove (retire) an analysis: dry-run by default, --apply to drop
    probe            Run the forensic probe battery against graph_events (22 probes, gated by
                     default; --output for the Markdown evidence report, --json for the raw
                     findings, --full-battery to run every probe ungated)
    probe-oracle     Recompute each checkable probe's declared population independently
                     and print a comparison + coverage/exclusion table (--output)
```

### `report` — Full options (Phase 12)

The `report` command supports Markdown/PDF output, entity-scoped reports, LLM narrative, and a pagination cap.

```
urban-integrity-cli report [OPTIONS]

OPTIONS:
    --ward <WARD>              Ward code (e.g. X05999001)
    --min-score <SCORE>        Minimum P_c score [default: 0.5]
    --motif <MOTIF>            Filter by motif. Pass the stored variant name:
                               PhoenixLoop | AddressCluster | NepotismBridge
    --from <DATE>              Start date (YYYY-MM-DD)
    --to <DATE>                End date (YYYY-MM-DD)
    --format <FORMAT>          Output format: table | json | md | pdf
                               (--output is an accepted alias)
    --output-file <PATH>       Write report to file (md and pdf formats only)
    --entity <NAME_OR_SLUG>    Entity-scoped report across all wards/LPAs
    --scope <SCOPE>            Cross-LPA correlated scope for --entity reports
                               (e.g. all-lpa)
    --summarize                Prepend LLM narrative via Ollama (graceful WARN if unavailable)
    --limit <N>                Maximum alerts to include; 0 = all [default: 100]
```

**Format notes:**
- `--format md` writes a GFM Markdown file. Default filename: `integrity-report-<scope>-YYYY-MM-DD.md`.
- `--format pdf` requires `pandoc` on PATH (falls back to `wkhtmltopdf` if pandoc cannot produce PDF directly). The `.md` file is always written alongside the `.pdf` and is preserved even if PDF generation fails.
- `--output-file` is only valid with `--format md` or `--format pdf`; other formats reject the flag.
- `--summarize` calls `http://localhost:11434/v1` (Ollama, `gemma4` model). Degrades gracefully with a WARN log if Ollama is unavailable.
- Alerts are ordered **by score descending** and filtered over the whole corpus, like
  `alerts list`. When `--limit` hides matches, `report` prints
  `Note: showing N of M matching alerts — use --limit N for more (--limit 0 for all).`
  on stderr.
- Withdrawn alerts are excluded from every report format: `report` builds its
  `AlertFilter` with `include_withdrawn: false` and offers no flag to override it.
- `--motif` is passed through to the query **unnormalised**, and the query compares it
  case-insensitively against the `MotifType` stored in `result_json`. Use `PhoenixLoop`,
  `AddressCluster` or `NepotismBridge`. The short forms `phoenix`/`cluster`/`nepotism` are
  translated only by `alerts list`, so `report --motif cluster` matches nothing and reports zero
  alerts rather than failing.

**Examples:**

```bash
# Markdown report for a ward, written to default filename
urban-integrity-cli report --ward X05999001 --format md

# PDF report written to a specific path
urban-integrity-cli report --ward X05999001 --format pdf --output-file /tmp/ward-report.pdf

# Entity-scoped report with LLM summary
urban-integrity-cli report --entity "Apex Build Ltd" --format md --summarize

# JSON output for scripting (all alerts, no cap)
urban-integrity-cli report --ward X05999001 --format json --limit 0
```

### `alerts` — Subcommands (Phase 11)

`alerts` requires a subcommand:

#### `alerts list` — List flagged motifs

```
urban-integrity-cli alerts list [OPTIONS]

OPTIONS:
    --min-score <SCORE>    Minimum P_c score [default: 0.5]
    --ward <WARD>          Filter by ward code (e.g. X05999001)
    --motif <MOTIF>        Filter by motif: phoenix | cluster | nepotism, or the
                           stored variant names PhoenixLoop | AddressCluster |
                           NepotismBridge
    --from <DATE>          Start date (YYYY-MM-DD)
    --to <DATE>            End date (YYYY-MM-DD)
    --since <DURATION>     Shorthand duration filter: Nh (hours) or Nd (days)
                           e.g. 24h, 7d  [conflicts with --from / --to]
    --limit <N>            Maximum results; 0 = unlimited [default: 100]
    --include-withdrawn    Also show alerts an operator has withdrawn
                           (hidden by default)
    --output <FORMAT>      table | json
```

`--since` accepts positive integer values only. Negative values are rejected. Format: `<N>h` (hours) or `<N>d` (days).

Results are ordered **by score descending** (then newest, then id), and every filter is
applied over the complete alert corpus rather than a recency window — see *The alert read
path* above. When `--limit` hides matches, the command says so on stderr rather than leaving
it to be inferred:

```
Note: showing 100 of 1947 matching alerts, highest score first — use --limit N for more, or --limit 0 for all.
```

`--limit 0` means unlimited and prints a warning first, since it fetches every match. Unlike
the CLI, neither API surface has an unlimited value and neither reports the total.

`alerts list` maps the short `--motif` names onto the variant names stored in
`result_json` before querying (`phoenix` → `PhoenixLoop`, `cluster` → `AddressCluster`,
`nepotism` → `NepotismBridge`). Without that mapping the filter compares e.g. `cluster` against
`AddressCluster` and never matches — the short names silently returned zero rows against a
database holding thousands of matching alerts. An unrecognised value is passed through unchanged,
so a typo still yields no match rather than being coerced to a motif the operator did not ask
for. `report` and both API surfaces do **not** perform this mapping.

#### `alerts show <ID>` — Full detail for one alert

```
urban-integrity-cli alerts show <ID> [OPTIONS]

ARGS:
    <ID>    Alert UUID

OPTIONS:
    --output <FORMAT>    table | json
```

Table output renders four blocks:
1. Header — ID, Motif, Score, Ward, Detected timestamp
2. Entities — resolved display names (director slugs and CH numbers expanded)
3. Description — `evidence_snapshot` from `result_json` with slugs replaced by display names
4. Score Breakdown — `s_phoenix`, `s_proximity`, `s_clustering`, `p_base`, `m_env`, `p_final`, `imd_rank`, `ward_code`

JSON output includes a `description_resolved` field alongside the raw `result_json`.

> `alerts show` resolves an ID directly and is not withdrawal-filtered, and the
> table rendering has no withdrawal banner — on a retracted alert the `withdrawn_at` and
> `withdrawn_reason` keys appear inside `result_json` (visible in `--output json`).

#### `alerts withdraw <ID>` — Retract an invalidated alert

```
urban-integrity-cli alerts withdraw <ID> --reason <TEXT>

ARGS:
    <ID>    Alert ID (the `event_id`, as shown by `alerts list`)

OPTIONS:
    --reason <TEXT>    Why it is being withdrawn — recorded alongside the alert (required)
```

Marks the alert withdrawn by writing `withdrawn_at` (UTC, RFC 3339) and `withdrawn_reason` into
its stored `result_json`. It never deletes the row.

Behaviour:

- Unknown ID → `Alert <id> not found` on stderr, exit code `1`.
- Already withdrawn → refused, printing the original timestamp and reason, exit code `1`.
  Re-withdrawing would overwrite the audit trail the mark exists to preserve.
- An update matching no rows is a hard error, not a silent success.

On success it prints:

```
Alert <id> withdrawn: <reason>
It is hidden from `alerts list`, reports and the API; `alerts list --include-withdrawn` still shows it.
```

**Examples:**

```bash
# Recent alerts in the last 24 hours, table output
urban-integrity-cli alerts list --since 24h

# High-risk phoenix alerts in a ward, last 7 days
urban-integrity-cli alerts list --ward X05999001 --motif phoenix --min-score 0.8 --since 7d

# Full detail for a specific alert
urban-integrity-cli alerts show a1b2c3d4-e5f6-7890-abcd-ef1234567890

# Alert detail as JSON for scripting
urban-integrity-cli alerts show a1b2c3d4-e5f6-7890-abcd-ef1234567890 --output json

# Retract an alert a later data correction invalidated
urban-integrity-cli alerts withdraw a1b2c3d4-e5f6-7890-abcd-ef1234567890 \
  --reason "rests on a name-only officer merge; split 2026-08-11"

# Audit what has been retracted — the only surface that shows withdrawn alerts
urban-integrity-cli alerts list --include-withdrawn
```

### `anomalies` — Adjudicate Refused Inputs

An anomaly is **not** an alert. An alert says *"this looks like corruption"*; an anomaly says
*"this input could not be trusted, and here is exactly what was skipped."* They are kept in
separate tables (`data_anomalies` vs `graph_events` `MotifDetected`) and on separate commands so
data-quality noise never enters an analyst's corruption queue, and so a genuine finding cannot
hide among parse failures. **No API surface exposes anomalies** — there is no GraphQL field and
no gRPC RPC for them; this command is the only reader.

```
urban-integrity-cli anomalies <SUBCOMMAND>

SUBCOMMANDS:
    list             List open anomalies, highest severity first
    show <ID>        Show the full evidence for one anomaly
    review <ID>      Mark an anomaly as adjudicated
```

#### `anomalies list`

```
urban-integrity-cli anomalies list [OPTIONS]

OPTIONS:
    --kind <KIND>    Only this kind, e.g. officer_key_collision
    --all            Include reviewed and dismissed rows (open rows only by default)
```

#### `anomalies show <ID>`

```
urban-integrity-cli anomalies show <ID>

ARGS:
    <ID>    Anomaly record id or key
```

An anomaly key legitimately contains colons — it is built from the source
(`backfill:officers:audit`) and the subject (`director:smith-…`) — so the id parser strips only
the `data_anomalies:` table prefix rather than splitting on the first colon. Splitting on the
first colon truncated the key and reported every real anomaly as absent.

#### `anomalies review <ID>`

```
urban-integrity-cli anomalies review <ID> [OPTIONS]

ARGS:
    <ID>    Anomaly record id or key

OPTIONS:
    --dismiss    Record it as not worth acting on, rather than accepted
```

**Examples:**

```bash
# What did the pipeline refuse to guess at?
urban-integrity-cli anomalies list

# Only officer-key collisions, including ones already adjudicated
urban-integrity-cli anomalies list --kind officer_key_collision --all

# Full evidence for one
urban-integrity-cli anomalies show "backfill:officers:audit:director:smith-jane"

# Close it out
urban-integrity-cli anomalies review "backfill:officers:audit:director:smith-jane" --dismiss
```

### `entity` — Entity Profile (Phase 10)

Look up a company by name (case-insensitive substring match), slug, or Companies House registration number.

```
urban-integrity-cli entity [OPTIONS] [QUERY]

ARGS:
    [QUERY]    Company name (substring match) or slug (exact ID lookup).
               Mutually exclusive with --ch-number.

OPTIONS:
    --ch-number <CH>    Companies House registration number (exact match, 2–10 alphanumeric chars)
    --output <FORMAT>   table | json
    --deep              Include sub-candidate OSINT links (0.50–0.69 confidence,
                        investigative only). Read-only — these links cannot be
                        confirmed via the `links` CLI.
```

CH number validation: 2–10 ASCII alphanumeric characters. Injection characters (hyphens, quotes, semicolons) are rejected at the CLI boundary before any database call.

**Lookup behaviour:**
- If `QUERY` looks like a slug (lowercase, hyphenated, e.g. `apex-build-ltd`), an exact ID lookup is attempted first; falls back to substring name search on no match.
- Name search is a case-insensitive substring filter over Entity nodes — it is **not** a phonetic or similarity match. In-app fuzzy/phonetic matching was removed across the engine (2026-06-13); the project uses exact-identifier joins for identity resolution.
- If multiple name matches are found in table mode, a disambiguation list is printed. Use `--ch-number` for an exact lookup.
- `--format md` and `--format pdf` are not supported by `entity` — use `report --entity` instead.

**Table output sections** (single match):
1. Header block — name, CH number, status, incorporation date
2. Directors — list of known directors
3. Contracts — Contracts Finder and planning application records
4. LPA Presence — LPA codes with contract/application counts
5. Addresses — registered and operational addresses
6. Active Alerts — motif alerts referencing this entity
7. Related Entities — entities sharing a director or address
8. OSINT Enrichment — beneficial owners, disqualifications, and (with `--deep`) sub-candidate links

**Examples:**

```bash
# Look up by name (table output, default)
urban-integrity-cli entity "Apex Build Ltd"

# Look up by CH number
urban-integrity-cli entity --ch-number 12345678

# Get JSON and filter with jq
urban-integrity-cli entity "Apex Build Ltd" --output json | jq '.directors[].name'

# Extract incorporation date
urban-integrity-cli entity --ch-number 12345678 --output json | jq '{name, incorporationDate}'

# Use a director slug seen on another company profile
urban-integrity-cli entity apex-build-ltd

# Include investigative sub-candidate OSINT links
urban-integrity-cli entity "Apex Build Ltd" --deep

# Find related entities sharing a director connection
urban-integrity-cli entity "Apex Build Ltd" --output json \
  | jq '.relatedEntities[] | select(.sharedConnection.type == "director")'
```

### `graph-path` — Shortest Path Between Two Entities (Phase 13)

Find the shortest path between two entities in the HIN graph using BFS traversal over
`graph_events`. The graph is traversed as undirected; the maximum hop depth is 6.

```
urban-integrity-cli graph-path <FROM> <TO> [OPTIONS]

ARGS:
    <FROM>    Starting entity — name (substring match) or slug/CH-number (exact)
    <TO>      Target entity — name (substring match) or slug/CH-number (exact)

OPTIONS:
    --output <FORMAT>    table | json
```

If `FROM` or `TO` resolves to multiple entities, the command prints a disambiguation list
and exits. Re-run using a slug or Companies House number for an exact lookup.

Table output renders the path as:
`Name → EDGETYPE → Name → EDGETYPE → Name`

JSON output includes a `nodes` array (each with `id` and `name`) and an `edges` array
(each with `type`).

**Examples:**

```bash
# Trace a company to a councillor
urban-integrity-cli graph-path "Apex Build Ltd" "John Smith"

# Use slugs for exact disambiguation
urban-integrity-cli graph-path GB-COH:12345678 director:john-smith

# JSON output for scripting
urban-integrity-cli graph-path "Apex Build Ltd" "John Smith" --output json
```

### `status` — Engine Statistics (updated)

```
urban-integrity-cli status [OPTIONS]

OPTIONS:
    --output <FORMAT>    table | json
```

Returns the database name, `graph_events` count, projection state (`Complete` / `Unfinished` / `NeverRun`, from `projection_runs:current`) and the projection's node, edge and skipped-edge counts. `--format md` and `--format pdf` are not supported.

### `ingest-planning` — On-demand Planning Portal Scrape

Scrape one or all configured planning portals immediately, writing new graph events to SurrealDB. Deduplicates against existing `graph_events` records before writing.

```
urban-integrity-cli ingest-planning [OPTIONS]

OPTIONS:
    --ward <WARD_CODE>    Scrape a single configured ward (e.g. X05999001)
    --all                 Scrape all portals in [[ingest.planning_portals]] config
```

`--ward` and `--all` are mutually exclusive. At least one must be supplied.

Requires SurrealDB to be running and at least one `[[ingest.planning_portals]]` entry in `config.toml`. If the Scrapling sidecar is unreachable, the Firecrawl fallback is attempted per portal.

Output is a summary table: Ward Code | Applications | Written | Skipped | Status.

**Examples:**

```bash
# Scrape a single ward
urban-integrity-cli ingest-planning --ward X05999001

# Scrape all configured portals
urban-integrity-cli ingest-planning --all
```

### `backfill` — Enrich Graph Data (Phase 13)

Backfill enrichment data into the live graph. Requires a running SurrealDB instance.

```
urban-integrity-cli backfill <SUBCOMMAND>

SUBCOMMANDS:
    ch          Link planning applicants to Companies House numbers
    ward        Resolve ward codes for Contracts Finder contracts via postcodes.io
    planning    Re-scrape wards still attributed to the unknown-applicant
                placeholder and stamp applicant provenance across the planning corpus
    cessation   Fetch real date_of_cessation from Companies House so Phoenix's
                window is anchored where the company actually failed
    officers    Fetch officers for Companies House entities that have none, keyed
                on the identity evidence CH publishes rather than on the name alone
    psc-links   Re-run the PSC↔officer identity join over data already in the
                graph — no Companies House API calls
```

Every backfill is a long single-connection job, so the command holds a re-authenticating
SurrealDB session: the HTTP token expires after roughly an hour and a run that outlives it would
otherwise die with `401 Unauthorized`. The session re-selects the database that `--analysis`
resolved to, not the raw config default, so a mid-flight run cannot be moved onto a different
analysis by a token refresh.

#### `backfill ch` — Link Applicants to Companies House

Scans planning applicant entities that lack a CH number and attempts to match them via the
Companies House REST API. Matching is **exact-only** — the former edit-distance fuzzy matcher
was removed (2026-06-13); `match_company` now returns either an exact match or no match, and an
ambiguous name resolves to no match rather than an arbitrary pick.

**Requires the REST key `URBAN__ENRICHER__CH__API_KEY`** — not the streaming key. Companies House
issues two non-interchangeable credentials: the streaming key
(`URBAN__INGEST__CH_API_KEY`) authenticates `stream.companieshouse.gov.uk`, while this command
calls `api.company-information.service.gov.uk/search/companies`, which rejects it with `401`. The
command exits with an explanatory error if the REST key is absent or empty. See CONFIGURATION.md
for both variables.

```
urban-integrity-cli backfill ch [OPTIONS]

OPTIONS:
    --dry-run              Show what would be done without writing to the DB
    --rate-limit <N>       CH REST API requests per second [default: 2]
    --restart              Discard the saved resume cursor and walk every
                           candidate again
```

The default rate limit is `2`, matching the published Companies House allowance of 600 requests
per 5 minutes. An interrupted run resumes from a saved cursor by default, because a
full pass is several hours of lookups; pass `--restart` when the candidate set has changed under
a stale cursor, or to re-test names that previously found no match.

Output: `CH backfill: N scanned, M matched, X skipped, Y errors [DRY RUN]`

```bash
# Run CH backfill at the default rate (2 req/s, matching the CH allowance)
urban-integrity-cli backfill ch

# Preview without writing
urban-integrity-cli backfill ch --dry-run

# Ignore the saved resume cursor and re-walk every candidate
urban-integrity-cli backfill ch --restart
```

#### `backfill ward` — Resolve Ward Codes

Scans contracts that lack a ward code and resolves them by looking up their postcodes via
`postcodes.io`. No API key required.

```
urban-integrity-cli backfill ward [OPTIONS]

OPTIONS:
    --dry-run    Show what would be done without writing to the DB
```

Output: `Ward backfill: N scanned, M matched, X skipped, Y errors [DRY RUN]`

```bash
# Run ward backfill
urban-integrity-cli backfill ward

# Preview without writing
urban-integrity-cli backfill ward --dry-run
```

#### `backfill planning` — Repair planning applicant attribution

Re-scrapes the planning wards whose applications are still attributed to the
`director:unknown-applicant` placeholder, and stamps explicit applicant provenance across the
planning corpus. Reads the configured `[[ingest.planning_portals]]` entries and goes through the
Scrapling sidecar, so the sidecar must be reachable.

```
urban-integrity-cli backfill planning [OPTIONS]

OPTIONS:
    --dry-run    Show what would be done without writing to the DB
```

Output: `Planning backfill: N scanned, M matched, X skipped, B bulk-stamped, R already resolved, E errors [DRY RUN]`

#### `backfill cessation` — Anchor Phoenix on the real failure date

Fetches `date_of_cessation` from the Companies House REST API for entities the graph knows only
as "observed dissolved", so the Phoenix window is measured from when the company actually failed
rather than from when the stream first saw it.

```
urban-integrity-cli backfill cessation [OPTIONS]

OPTIONS:
    --dry-run              Answer only from the existing profile cache; uncached targets are
                           reported as skipped rather than fetched. Neither reads nor writes
                           the resume cursor, because it changes nothing
    --rate-limit <N>       CH REST API requests per second [default: 2]
    --restart              Discard the saved resume cursor and walk every candidate again
    --max-fetches <N>      Stop after this many network fetches, so a first live pass
                           can be bounded
```

Output: `Cessation backfill: N targets, M dated, X skipped, Y errors [DRY RUN]`

Requires the CH **REST** key `URBAN__ENRICHER__CH__API_KEY` (not the streaming key).

#### `backfill officers` — Evidence-keyed officer identity

Fetches officers for Companies House entities that have none, keying each `director:` node on the
identity evidence Companies House publishes (birth month, officer id) rather than on the name
alone — `director:{slug}-b{YYYYMM}` / `-o{id}`. Name-only keys conflate different people who
happen to share a name, which is exactly the defect that produced this project's one withdrawn
Phoenix alert.

```
urban-integrity-cli backfill officers [OPTIONS]

OPTIONS:
    --dry-run              Show what would be done without writing to the DB or spending
                           CH quota
    --rate-limit <N>       CH REST API requests per second [default: 2]
    --restart              Discard the saved resume cursor and walk every candidate again
    --max-fetches <N>      Stop after this many network fetches
    --audit                Audit the officer keys ALREADY in the graph instead of ingesting
                           new ones. Records an `officer_key_collision` anomaly for each key
                           that conflates people CH says are different, flagging separately
                           those already fused — read them back with `anomalies list`
    --split-collisions     Re-key officer events off the uncorroborated colliding keys
                           `--audit` reports, onto the CH officer id their company (or their
                           appointment date and role) attributes them to. Events no CH
                           evidence attributes are left where they are and reported, never
                           guessed at. Conflicts with `--audit`
```

Output: `Officer backfill: N uncovered companies, M appointments written, X skipped, Y errors [DRY RUN]`

`--split-collisions` rewrites `graph_events`, so it is a data correction rather than an
ingestion: run it with `--dry-run` first and `project` afterwards.

#### `backfill psc-links` — Re-run the PSC↔officer join

Re-runs the PSC↔officer identity join over data **already in the graph**. The join normally
happens inside the `ch_psc` adapter's fetch loop, so correcting the matcher would otherwise mean
re-fetching every PSC record to re-derive links from data that is already staged. This subcommand
makes no HTTP calls and needs no API key. It is idempotent: existing `ch_psc` links are skipped,
not duplicated.

```
urban-integrity-cli backfill psc-links [OPTIONS]

OPTIONS:
    --dry-run    Count the links that would be written without writing them
```

Output: `PSC-link backfill: N PSCs scanned, M matched an officer, A already linked, W written, R rejected on company number [DRY RUN]`

```bash
# Count what the corrected matcher would link, without writing
urban-integrity-cli backfill psc-links --dry-run

# Apply it
urban-integrity-cli backfill psc-links
```

### `project` — Project WAL into Native SurrealDB Tables (Phase 13)

Reads all `NodeAdded` and `EdgeAdded` events from the `graph_events` WAL and projects them
into typed native SurrealDB tables. This enables direct SurrealQL queries and visual
exploration in tools such as Surrealist.

**Warning:** This command wipes all native node and relation tables before reprojecting.
It is designed for idempotent re-runs — running it multiple times is safe.

```
urban-integrity-cli project [OPTIONS]

OPTIONS:
    --dry-run    Placeholder flag — not yet implemented.
                 Prints a message and exits without writing.
```

Output: `Done: N nodes upserted, M edges related, X nodes skipped, Y edges skipped`

```bash
# Project WAL into native tables
urban-integrity-cli project

# --dry-run is accepted but currently a no-op placeholder
urban-integrity-cli project --dry-run
```

### `export-graphml` — Export Graph as GraphML

Export the HIN graph as a spec-compliant GraphML file suitable for import into Gephi,
Cytoscape, yEd, or any tool that understands the GraphML 1.0 schema.

```
urban-integrity-cli export-graphml [OPTIONS]

OPTIONS:
    -o, --output <PATH>       Output file path [default: graph.graphml]
    --source <SOURCE>         Graph source: events | native [default: events]
    --reachable-from-wards [<CODE>...]
                              Restrict the export to the weakly-connected component
                              reachable from a set of ward seed nodes. Comma- or
                              space-separated; with no codes, falls back to config
    --node-type <TYPE>        Keep only nodes of this type (repeatable, case-insensitive)
    --edge-type <TYPE>        Keep only edges of this type (repeatable, case-insensitive)
    --drop-isolated           After type filtering, drop nodes left with no edges
```

#### `--source` — and why the default changed

| Value | Reads | Needs `project` first? |
|-------|-------|------------------------|
| `events` (**default**) | the `graph_events` WAL (`NodeAdded` + `EdgeAdded`) over the raw HTTP `/sql` endpoint | No |
| `native` | the projected native tables, via the SurrealDB SDK | Yes |

`native` was the default until **commit `722b568` (2026-08-25)**. It goes through the SurrealDB
SDK, whose flatbuffers codec fails to decode typed `RecordId` fields on large result sets —
measured on the 358k-node Anglesey analysis, it errors with `Failed to deserialize flatbuffers
payload`. `events` reads raw HTTP JSON, where every id (including coerced `director:` keys)
arrives as a string, and has no such ceiling. `native` is kept for small graphs and for exporting
exactly what `project` produced, but the default is now the path that works at real scale
(`crates/cli/src/commands/export_graphml.rs`, `ExportGraphmlArgs::source`).

`events` deduplicates nodes by `entity_id`, keeping the highest-`timestamp` row so correction
events win, then resolves `EdgeAdded` rows against the surviving node set. Both `NodeAdded` and
`EdgeAdded` reads page to exhaustion in 20,000-row pages (`raw_select_all_of_kind`); the previous
single-statement `LIMIT 100000` / `LIMIT 200000` ceilings were sized to an older graph and
truncated silently against 358,829 `NodeAdded` and 388,162 `EdgeAdded` rows — a truncated GraphML
still opens and renders, so the missing half looked like an absence of data rather than an
absence of rows.

Failure modes are explicit rather than empty: `native` against unprojected tables exits with

```
No nodes found in native tables — run `urban-integrity-cli project` first, or pass --source events.
```

and `events` against an unpopulated database exits with
`No NodeAdded events found in graph_events — is the engine populated?`

#### ⚠️ `--source events` ignores the global `--analysis` flag

**This is current, unfixed behaviour.** Only the `native` branch calls `connect_db`, which routes
through `resolve_analysis_db` (`crates/cli/src/db.rs:18`). The `events` branch calls
`load_from_events` (`export_graphml.rs`), which builds its own raw HTTP client from
`config.surreal.namespace` and `config.surreal.database` directly and never consults
`config.analysis`.

Because `events` is now the **default**, the consequences are:

- `urban-integrity-cli --analysis <id> export-graphml` silently exports whatever `[surreal].database`
  in `config.toml` names — **not** the analysis you asked for. There is no warning and no error;
  the file is written and looks correct.
- The catalog validation `resolve_analysis_db` performs is skipped entirely: an unknown analysis
  id does not error, and an analysis whose registry status is `retired` or `retiring` is not
  refused.

Until it is fixed, either pass `--source native` (which resolves the analysis correctly, at the
cost of the flatbuffers ceiling and a prior `project` run), or set the target database
explicitly for the invocation:

```bash
URBAN__SURREAL__DATABASE=<db_name> urban-integrity-cli export-graphml -o example.graphml
```

#### `--node-type` / `--edge-type` / `--drop-isolated`

Both type flags are repeatable and matched case-insensitively against the types actually present
in the loaded graph. Filtering runs after loading, in this order: nodes, then edges, then
`--drop-isolated`. Because `petgraph::filter_map` drops any edge whose endpoints did not survive,
an edge kept by `--edge-type` still disappears if `--node-type` removed one of its endpoints —
which is how the two flags compose.

**An unknown type is a hard error, not an empty export.** `resolve_types`
(`export_graphml.rs`) bails with the unknown values *and a sorted list of what is actually
present*, and nothing is written:

```
unknown node type(s): Offcier
  present in this graph: Contract, Entity, Location, Officer, Ward
Nothing was exported — a filter that matches nothing is a mistake, not an empty result.
```

That is deliberate: a typo would otherwise produce an empty graph indistinguishable from "this
analysis has no officers" — the same zero-output ambiguity that let the ICIJ adapter log 814k
rows written against an empty table for its entire life.

**The type vocabulary differs by source**, because the two loaders derive it differently:

| Source | `node_type` values | `edge_type` values |
|--------|--------------------|--------------------|
| `events` | the `NodeWeight` variant name from `weight_json.type` — `Entity`, `Officer`, `Location`, `Contract`, `Ward`, `PoliticalActor`, `Psc`, `Charity`, `Property`, `OffshoreEntity`, `Disqualification` | the `EdgeWeight` variant name — `Directs`, `LocatedAt`, `AwardedTo`, `IssuedBy`, `Represents`, `Controls`, `TrusteeOf`, `OwnsProperty`, `OffshoreLink`, `DonatesTo`, `DisqualifiedFrom`, `DeclaresInterest` |
| `native` | the source table name — `entity`, `officer`, `location`, `contract`, `ward`, `political_actor` (the OSINT node tables are **not** read) | the source relation table name — `directs`, `located_at`, `awarded_to`, `issued_by`, `represents` (the OSINT relation tables are **not** read) |

Case-insensitive matching means `--node-type officer` works against either source, but
`--node-type PoliticalActor` matches only under `--source events` and `--node-type
political_actor` only under `--source native`. When a value is rejected, read the "present in
this graph" list the error prints rather than guessing.

`--drop-isolated` is applied last, after both type filters. Without it a type-filtered export is
mostly unconnected dots in Gephi. After filtering, the command prints
`Filtered → N nodes, M edges` before writing.

**Output format:**

The generated file is a directed GraphML document with `<key>` declarations for all node
and edge attributes at the top of the file, before the `<graph>` element.

Fixed keys present on every node:

| Key ID | `for` | Attribute name | Description |
|--------|-------|----------------|-------------|
| `label` | `node` | `label` | Display name (falls back to `entity_id` if name is absent) |
| `node_type` | `node` | `node_type` | Node type, in the source's own vocabulary (see the table above) |

Fixed key present on every edge:

| Key ID | `for` | Attribute name | Description |
|--------|-------|----------------|-------------|
| `edge_type` | `edge` | `edge_type` | Edge type, in the source's own vocabulary |

Additional node attributes (prefixed `n_` in the `<key>` `id`) are emitted for all scalar
fields present on a given node type — for example `entity_status`, `company_number`,
`postcode`, `ward_code`, `value_gbp`, `officer_role`. The exact set depends on which fields
are populated in the database.

Additional edge attributes (prefixed `e_` in the `<key>` `id`) include any scalar fields
on the relation record plus flattened entries from the `data` sub-object, prefixed
`data_` — for example `data_officer_role`. When using `--source events`, top-level weight
fields and nested sub-objects (`meta.*`, `data.*`) are flattened one level, preserving
temporal metadata (`created_at`, `effective_from`, `effective_to`).

Node IDs in the GraphML file are the HIN `entity_id` values (e.g. `GB-COH:12345678`,
`director:john-smith`). All special XML characters in IDs and attribute values are escaped
(`&`, `<`, `>`, `"`).

**Examples:**

```bash
# Export to the default filename (graph.graphml) from the graph_events WAL — the default source
urban-integrity-cli export-graphml

# Export to a specific path
urban-integrity-cli export-graphml --output /tmp/hin-export.graphml
urban-integrity-cli export-graphml -o /tmp/hin-export.graphml

# Read the projected native tables instead (requires `project` first; honours --analysis)
urban-integrity-cli export-graphml --source native --output exports/projected.graphml

# Slice by type and drop what is left unconnected (events vocabulary: PascalCase)
urban-integrity-cli export-graphml \
  --node-type Officer --node-type Entity --edge-type Directs --drop-isolated \
  -o exports/directorships.graphml

# The same slice against native tables (table-name vocabulary: snake_case)
urban-integrity-cli export-graphml --source native \
  --node-type officer --node-type entity --edge-type directs --drop-isolated

# Targeting a named analysis: --analysis does NOT reach the default events source.
# Set the database explicitly instead.
URBAN__SURREAL__DATABASE=<db_name> \
  urban-integrity-cli export-graphml -o exports/example-$(date +%F).graphml
```

### `probe` — Forensic Probe Battery

Runs the registered battery of **22** forensic probes against `graph_events`, prints a verdict
table, and writes a Markdown evidence report. **A default run is gated** — a probe whose gate
predicate finds no qualifying upstream signal reports `SKIPPED` and never runs; `--full-battery`
opts out. Either way the battery returns exactly one `Finding` per registered probe, in
registration order. The battery is the Rust port of `scripts/analyst_probe.py`;
the Python script is still in the tree as the reference implementation, and the Rust registration
order deliberately mirrors its `OSINT_PROBES` list (`scripts/analyst_probe.py`, cited in
`crates/probe/src/run.rs`).

**`crates/api` does not expose probes.** There is no GraphQL field and no gRPC RPC for them.
Besides this command and `probe-oracle`, the workbench server runs the battery in-process
(`GET /api/analysis/report`, `POST /api/analysis/run`) — see `docs/WORKBENCH.md`.

```
urban-integrity-cli probe [OPTIONS]

OPTIONS:
    --output <PATH>    Where to write the Markdown evidence report
                       [default: evidence-report.md in the working directory]
    --json <PATH>      Write the full 22-`Finding` battery as pretty-printed JSON.
                       Independent of --output — a --json-only invocation still writes
                       the Markdown report to the default path
    --full-battery     Run every registered probe ungated. Gating is the DEFAULT; this flag
                       opts out so a gated run can always be compared against the complete
                       one. It selects the dispatch function only — the dependency graph is
                       validated on both paths, before any probe runs
```

Both flags create their parent directory when it is missing, and both artifacts are attempted
before either failure is reported: the battery has already done its work by that point, so an
unwritable `--json` path must not also cost the caller the Markdown report. A write failure is
still fatal, just reported after both attempts.

The command honours the global `--analysis` selector: `execute`
(`crates/cli/src/commands/probe.rs`) opens both connections it needs — the analysis database
via `connect_db` and `_catalog` via `connect_catalog` — from **one** `AppConfig::load()`, and
`identity-integrity` receives the same `resolve_analysis_db` value the primary connection used,
so the two cannot resolve onto different analyses.

```bash
# Run the battery against the configured analysis (writes ./evidence-report.md)
urban-integrity-cli probe

# Target a named analysis and choose both artifact paths
urban-integrity-cli --analysis <db_name> probe \
  --output reports/evidence.md --json reports/findings.json
```

#### The battery

Registration order is load-bearing, and `build_battery` (`crates/probe/src/run.rs`)
is its single site — `execute`, the oracle's coverage reconciliation and the dependency-graph tests
all read that one list, so a probe added there is immediately visible to every consumer instead of
drifting against a hand-copied subset. `tier()` is a per-probe constant returning exactly `Tier-1`
or `Tier-2`; it is not a runtime property of a finding.

**Not every registered probe runs.** `dispatch_battery` validates the dependency graph
unconditionally and then chooses a dispatch function: `graph::run_gated` by default, or the
unconditional `run_probes` loop (`crates/probe/src/lib.rs`) under
`--full-battery`. A gated-out probe still yields a `Finding` — verdict `Skipped`, carrying a typed
cause — so the 1:1 probe-to-finding correspondence holds under both modes. See *Gating* below.

| # | Probe name | Tier | What it measures |
|---|------------|------|------------------|
| 1 | `precomputed-alerts` | Tier-1 | `MotifDetected` alerts already in the graph. It reads the `detector_runs:current` marker first, but since ADJ-02 (phase 40) that marker **only qualifies the summary** — it never suppresses the count, because the five enricher post-adapter detectors write alerts without ever touching it, so marker absence does not mean no detector ran. The verdict is driven purely by the alert count; the summary states whether the Tier-B marker was present |
| 2 | `identity-integrity` | Tier-1 | Planning-applicant provenance coverage across `planning:` Contract nodes, compared against the `_catalog.analyses.coverage_baseline_json` baseline (1.0 percentage-point regression tolerance). The only probe needing a second connection |
| 3 | `detector-readiness` | Tier-1 | Which motif detector (Phoenix loop / Nepotism bridge / Address cluster) is starved, and by which specific missing term — never a generic node-type shortfall count. Since ADJ-02 it also carries the Tier-B "did the detector actually run" signal (marker present with its run timestamp / never ran / read failed) as an evidence row on every exit path — annotation only, never a gate on its own verdict |
| 4 | `address-cluster-exact` | Tier-1 | Two or more distinct planning applicants sharing exactly one registered address. Reports `Blocked` when probe 2 set `Strategy.identity_blocked` |
| 5 | `geographic-concentration` | Tier-1 | Repeated streets and postcode sectors across distinct `Location` **entities** (see the ADJ-01 note below) |
| 6 | `ward-concentration` | Tier-1 | Contracts per ward, deduplicated in Rust over `Represents` edges |
| 7 | `temporal-burst` | Tier-1 | Busiest ingestion day, always stated against the total Contract population — of which only rows carrying a usable timestamp are `capable` |
| 8 | `identity-link-registry` | Tier-2 | Whether `identity_links` holds any confirmed or pending resolutions at all — the gate every later Tier-2 probe's `Strategy` reads |
| 9 | `osint-evasion-alerts` | Tier-2 | A `disqualified_director_evasion` `MotifDetected` alert for a director under a live Companies House disqualification order |
| 10 | `osint-disqualified-directors` | Tier-2 | A disqualified director linked to an entity that currently holds a contract award (a CDDA 1986 violation) |
| 11 | `psc-beneficial-ownership` | Tier-2 | PSC beneficial-ownership records for entities in the procurement graph, and whether any indicate control by a disqualified or sanctioned individual |
| 12 | `gazette-insolvency-signals` | Tier-2 | Entities the London Gazette reports in liquidation that still hold a live contract award |
| 13 | `property-nodes` | Tier-2 | `Property` nodes from either CCOD or OCOD, checked against a per-producer `enrichment_runs` marker rather than CCOD alone |
| 14 | `phase20-ec-donations` | Tier-2 | Electoral Commission donation records for entities also present in the procurement graph |
| 15 | `phase20-council-registers` | Tier-2 | Council-registers-attributable `PoliticalActor` / `DeclaresInterest` evidence, distinguished from `electoral_commission`'s own `PoliticalActor` nodes and from `planning_identity`'s `Directs` edges |
| 16 | `osint-confirmed-intel` | Tier-2 | Confirmed or auto-linked OSINT identity resolutions, and whether any carry a high-risk resolution type |
| 17 | `osint-analyst-queue` | Tier-2 | Identity links sitting at 0.70–0.84 confidence awaiting analyst sign-off, highest-confidence first |
| 18 | `osint-new-node-types` | Tier-2 | Whether the newer OSINT-sourced node types (`Psc`, `Charity`, `Property`, `OffshoreEntity`, `InLiquidation`) appear at all. Evidence renders as three labelled rows — OSINT nodes added (4 terms), OSINT status updates (`InLiquidation`, 1 term), and the 5-term total — so a node count is never silently merged with a status-update count. The total is untouched by that split, which is what keeps it comparable against the oracle |
| 19 | `osint-risk-edges` | Tier-2 | Whether disqualified-director edges intersect other OSINT signals — `Controls`, `OffshoreLink`, donation, `TrusteeOf` |
| 20 | `land-flip-candidates` | Tier-2 | Gate-1 survivors that also pass gate-3 corroboration, split by structural CCOD-vs-individual capability |
| 21 | `donation-contract-correlation` | Tier-2 | Whether a donation correlates in time (within 730 days, either direction) with an award from a party-controlled LPA to the same entity |
| 22 | `opencorporates-cross-jurisdiction` | Tier-2 | Foreign-registered entities tied to the procurement graph. Constructed with the adapter's own enablement state — `enabled`, `oc_open_data_confirmed` and a `.trim()`-non-empty API key — so it cannot report a verdict for a source that never actually runs |

Probes 2 → 4 are the one ordering-critical cross-probe dependency: `identity-integrity` writes
`Strategy.identity_blocked` and `address-cluster-exact` reads it, which is why position 2 must
precede position 4. It is also the only declared `depends_on()` edge in the whole battery, so the
dependency graph below has exactly one edge in it.

`Strategy` also carries a lazy `enrichment_runs` memo, filled by whichever probe asks
first and reused by every later probe in the same dispatch regardless of order; a scan
failure degrades it to an empty map, which reads as "no marker for this source" and never as a
measured "definitely ran".

> **ADJ-01 (fixed 2026-09-01, commit `c8fb0c7`).** `geographic-concentration` used
> `scan::scan_entities_by_type`, which returns one row per `NodeAdded` **event**, where it needed
> `scan::scan_latest_entities_by_type`, one row per **entity**. A re-ingested `Location` therefore
> contributed once per event, and a duplicated entity could cross the "≥ 2 applications on a
> street" threshold on its own duplicates alone. Measured live on `an_anglesey_aebaa785`: 102,897
> `Location` `NodeAdded` events against 61,756 distinct `Location` entities. After the fix the
> probe's `matches` figure went 23,322 → 1,697, matching the Python reference exactly. Those two
> numbers are `matches` (streets with ≥ 2 applications), **not** populations — earlier phase docs
> conflated the two.

#### Gating — which probes actually run

`crates/probe/src/graph.rs`.

**The dependency graph.** Every probe declares `depends_on() -> &'static [&'static str]`, defaulting
to empty. `validate_dependency_graph` builds the graph from the registered battery and fails the
command — before any probe runs, on both dispatch paths — when a declared dependency names an
unregistered probe, when the declarations form a cycle, or when the implied dispatch order
interleaves the two tiers. Order comes from a hand-rolled Kahn's topological sort whose ready queue
is re-sorted every pass rather than held in a set, so two probes with no path between them always
emerge in registration order. Exactly one edge exists today: `address-cluster-exact` →
`identity-integrity`.

**The gate predicate.** Evaluated per probe, in dispatch order, against the findings accumulated so
far:

1. Every **declared dependency** must have returned `Hit` or `Weak` — an AND across the probe's own
   `depends_on()` list. Otherwise the probe is `Skipped` with a `Dependency` cause naming the
   upstream and the verdict that upstream actually returned.
2. A **Tier-2** probe additionally requires that *at least one* Tier-1 probe returned `Hit` or
   `Weak` — an OR across the tier, not an AND. Otherwise it is `Skipped` with a `TierAggregate`
   cause recording the tier and how many Tier-1 probes were checked. This rule reads `tier()` at
   gate time and is deliberately not modelled as graph edges.

The tier rule is exactly why an interleaved dispatch order is rejected at validation: the OR is
computed over probes dispatched so far, so every Tier-1 probe must dispatch before the first Tier-2
probe — otherwise a Tier-2 probe could be gated out against an incomplete Tier-1 result set, which
is evidence suppressed by a wrong predicate rather than by a real absence of signal.

`run_gated` dispatches topologically but writes each finding back into its input slot, so the
returned findings are in registration order regardless of the order they were produced in. It
refuses to dispatch when the graph it was handed was not built for the exact probe slice being run
(`ensure_built_for`, compared positionally by name) — a stale graph answers "no dependencies" for a
probe it has never seen, which would silently under-gate.

**`--full-battery`** runs `run_probes` instead: every probe, unconditionally, no gate, no skip.
Validation still runs. `probe-oracle` always dispatches in this mode — see below.

#### Verdicts

Each probe returns a `Finding`, whose fields are `probe`, `verdict`, `summary`, `cause`
(`Option<Cause>`), `severity`, `evidence`, `attributions` and `remediation_hint`
(`crates/schema/src/types/verdict.rs`). `Verdict` has exactly seven values:

| Verdict | Carries `Coverage`? | Meaning |
|---------|---------------------|---------|
| `HIT` | yes, plus `matches` | matches at or above the probe's hit threshold |
| `WEAK` | yes, plus `matches` | at least one match, below the threshold |
| `CLEAR` | yes | zero matches over a fully-capable population |
| `NOT_APPLICABLE` | yes, plus a required `reason` | the structurally-capable subset was smaller than the population |
| `DATA_GAP` | no | nothing was measured — the input is missing |
| `BLOCKED` | no | a prerequisite probe's result makes this measurement untrustworthy |
| `SKIPPED` | no | the gate predicate decided the probe should not run at all; carries a typed `SkipCause` |

`Coverage` is a `population` / `capable` pair, and the table prints it beside every verdict that
carries one (e.g. `HIT (412/412, matches=7)`, `NOT_APPLICABLE (900/412) — …`).

**`Clear` is unconstructible except through `Verdict::classify`**, which returns `NotApplicable`
whenever `capable < population`. `Coverage`'s fields are private and its only constructor runs
inside `classify`, so `Verdict::Clear(Coverage { population: 5, capable: 3 })` in another module
is a compile error (E0451), not a runtime assertion. `classify` additionally **panics** when
`capable > population` — a capable subset cannot exceed the population it was drawn from, so that
is a caller bug rather than a data condition to clamp away. `Coverage`, `Verdict`, `Finding`,
`Cause` and `Severity` derive `Serialize` only and deliberately not `Deserialize`, so the same
invariant-violating value cannot be smuggled in from JSON either. The result is that a probe
cannot report "nothing suspicious found" about a population it never measured.

`Skipped` carries **no** `Coverage`, for the same reason `DataGap` and `Blocked` do not and then
some: the probe never ran, so there is no population it examined. A `population: 0, capable: 0`
there would render as "checked nothing and found nothing", the exact conflation the BLUF ordering
exists to prevent. Its `SkipCause` wraps a *module-private* inner enum with two variants —
`Dependency { upstream, upstream_verdict }` and `TierAggregate { tier, probes_checked }` — so the
payload cannot be fabricated from outside `schema::types::verdict` at all (a strictly stronger
guarantee than `Coverage`'s private fields, since a public enum's variant fields would otherwise be
public). The recorded `VerdictKind` is always derived from a real upstream `&Verdict` by
`VerdictKind::of`, never asserted by the caller. What this does **not** prevent is a probe
self-reporting a skip from its own `run()` — the same residual `Verdict::blocked` and
`Verdict::data_gap` already have. `SkipCause::describe()` is the single place the sentence
("upstream 'identity-integrity' returned Clear", "no Tier-1 probe (of 7 checked) returned Hit or
Weak") is authored; the table, the report and the JSON all render from it rather than re-deriving
it from prose.

`Severity` is a five-value vocabulary — `Critical`, `High`, `Medium`, `Low`, `Info` — matching the
Python script's own severity strings 1:1.

A probe that hits an internal error — a failed query, a missing table — catches it inside its own
`run` and returns a `DATA_GAP` or `BLOCKED` finding. `Probe::run` does not return `Result`, so one
failing probe can never abort the battery.

**Console output** is a three-column table: `Probe | Verdict | Summary`, verdicts coloured by
severity (HIT red, WEAK yellow, CLEAR green, NOT_APPLICABLE grey, DATA_GAP dark grey, BLOCKED
magenta, SKIPPED cyan).

#### The evidence report

`evidence-report.md` is now assembled in Rust by `report::render_evidence_report`
(`crates/probe/src/report/mod.rs`), not by `scripts/analyst_probe.py`. Sections are emitted
in this fixed order:

1. Header — title and a `chrono::Utc::now()` RFC 3339 timestamp
2. Bottom Line Up Front (`bluf`) — structural blindness first: how many probes could not evaluate
   at all (`DataGap` + `Blocked`), then how many the gate predicate skipped, then aggregate capable
   vs population, and only then the confirmed-hit count. The skip line is always printed, including
   as "0 probes skipped", so a gated run's shorter report can never be mistaken for a cleaner one
3. Verdict Legend (`legend`) — one row per `Verdict` variant; seven rows since `Skipped` was added
4. Skipped Probes (`skipped`) — one bullet per gated-out probe, rendered from the typed
   `SkipCause::describe()` rather than re-derived from summary prose. Placed here, directly after
   the legend, so it precedes the findings a reader may stop reading at. Always rendered: with
   nothing skipped it states "No probes were skipped in this run"
5. How the Search Adapted (`adapted`) — what the battery's `Strategy` changed as it ran
6–10. Per-verdict detail groups, most-actionable first: **Confirmed Findings** (`Hit`),
   **Suggestive Patterns** (`Weak`), **Data-Integrity Gaps** (`DataGap` + `Blocked`),
   **Not Applicable** (`NotApplicable`) and **Verified Negative Findings** (`Clear`). `Skipped` is
   deliberately not a group here — a skipped probe measured nothing, so it has no "what was
   measured" summary to render under a heading — and section 4 is its one home. Every one of the
   seven `Verdict` variants lands in exactly one of those two places, which a non-vacuous test pins
11. Full Probe Ledger (`ledger`) — `Tier | Probe | Verdict | Severity | Population | Capable | Provenance`
12. Contradiction checks (`contradictions`)
13. Recommended Remediation (`remediation`)

Each detail entry renders the probe name, its `hypothesis()` as a blockquote, its summary, and its
evidence rows. Entries under **Verified Negative Findings** additionally carry a population-source
sentence — `oracle-corroborated — independently recomputed population = N`, or `self-reported only,
not independently checked` with the exclusion reason — because a `Clear` over a population nothing
independently checked is a materially weaker claim than one over a corroborated population. The
Ledger's `Provenance` column is the compact form of the same fact: `Oracle: N`, `Self-reported`, or
`-` where no population was measured.

### `probe-oracle` — Independent Population Comparison

`probe` reports each probe's *declared* population — the count that probe computed for itself.
`probe-oracle` recomputes that number a second time by an independent route and prints the two
side by side on one row, so a disagreement is visible rather than inferable. It exists because the
correct Contract population (39,860, ward-reachable) sat on disk for four days while three probes
reported 99,599 and nothing compared them.

```
urban-integrity-cli probe-oracle [OPTIONS]

OPTIONS:
    --output <PATH>    Write a rendering of both tables to this path. Without it,
                       output goes to stdout only
```

`--output` is the command's only flag. It honours the global `--analysis` selector.

The command runs the real 22-probe battery via `probe::run::execute` — with `output` and `json`
both `None`, so it writes no evidence report and no findings JSON of its own, and with
`full_battery: true`, so **its run is never gated**. That is deliberate: a gated Tier-1 `Clear`
would leave most Tier-2 rows reading `not measured this run` and silently empty the command of the
comparison it exists to make. It then opens its own connection and recomputes every checkable
probe's population through `urban_integrity_core::persistence::oracle` (documented below).

The analysis database is resolved **before** the battery and re-checked **after** it. For a
non-`main` analysis that resolution is a `_catalog` registry lookup rather than a config read, and
the battery is a long window; an `analysis` retire or repoint inside it would send the oracle's
queries at a different database than the probes read, fabricating a disagreement on every row. A
mismatch aborts with a re-run instruction instead of printing a comparison.

When at least one checked probe disagrees, a pointer line naming `docs/ADJUDICATIONS.md` prints
after the tables and is mirrored into the `--output` artifact — and only then, never on a run where
everything agreed.

> **`--output` artifacts are plain text (WR-03, fixed).** The file used to be a rendering of the
> same `comfy_table` values printed to stdout, so it picked up ANSI escape codes whenever the
> command ran interactively — neither plain text nor byte-stable between runs, which defeated
> diffing the artifact. Both tables are now re-rendered through `force_no_tty()` for the file, so
> the artifact is diffable as written. (Finding WR-03 in
> the phase 38 code review.)

```bash
# Compare against the configured analysis, stdout only
urban-integrity-cli probe-oracle

# Against a named analysis, with an artifact on disk
urban-integrity-cli --analysis <db_name> probe-oracle --output reports/oracle.txt
```

#### Table 1 — comparison

Columns: `Probe | Declared | Oracle | Delta | Agrees`. One row per checkable probe, declared and
oracle counts always on the **same** row.

- `Agrees` is `AGREE` (green) when `|declared − oracle| <= delta_allowed`, `DISAGREE` (red)
  otherwise.
- `Delta` comes from the `PROBE_DELTA` table, which is **empty today** — every checked probe
  therefore has a tolerance of `0` and is held to exact agreement. A non-zero entry is required to
  carry a non-empty justification string, checked by `probe_delta_is_justified`.
- When a probe's verdict this run was `DataGap` or `Blocked`, nothing was measured, so `Declared`
  and `Agrees` both render `not measured this run` rather than comparing against a false `0`. The
  same is structurally true of `Skipped`, which carries no `Coverage` either — but this command's
  ungated run cannot produce one.

The **10 checked probes**, declared in `CHECKED_PROBES`
(`crates/probe/src/population_source.rs` — the same module supplies each finding's
`Provenance` value in the evidence report), and how each population is independently recomputed:

| Probe | Independent recomputation |
|-------|---------------------------|
| `precomputed-alerts` | `count_events(kind = "MotifDetected")`, no markers |
| `identity-integrity` | `count_distinct_entities` on `NodeAdded` with marker `"type":"Contract"` and prefix `planning:` — the canary row: this must equal the independently-proven ward-reachable Contract population |
| `address-cluster-exact` | `count_distinct_edge_field` with marker `"type":"LocatedAt"` and `EdgeField::Dst` |
| `geographic-concentration` | `count_distinct_entities` on `NodeAdded` with marker `"type":"Location"`, no prefix |
| `ward-concentration` | `count_distinct_entities` on `NodeAdded` with marker `"type":"Contract"`, no prefix |
| `temporal-burst` | the same distinct-Contract-entity count as `ward-concentration` (it dedups by earliest event rather than latest, but the count is identical) |
| `gazette-insolvency-signals` | `count_events("NodeUpdated", markers = ["InLiquidation"])` — a plain substring needle, matching the probe's own |
| `property-nodes` | `count_events` on `NodeAdded` with marker `"type":"Property"` |
| `osint-new-node-types` | the sum of five `count_events` calls: `Psc`, `Charity`, `Property`, `OffshoreEntity` `NodeAdded` plus `InLiquidation` `NodeUpdated` |
| `phase20-council-registers` | `count_events` on `NodeAdded` with the two markers `"type":"PoliticalActor"` and `"council_name":"` — the AND approximating the probe's own council-name discriminator |

One known asymmetry that is *not* a data-integrity defect, and one adjudication that has since
been closed:

- **`address-cluster-exact`** — the probe counts a `LocatedAt` destination only when **both**
  `src_id` and `dst_id` parse via `record_id_str`; the oracle counts distinct `dst_id` regardless
  of whether `src_id` parses. A live edge with an unparseable `src_id` but a valid unique
  destination therefore produces a real `oracle > declared` gap that is an artifact of the two
  counting shapes.
- **`osint-new-node-types`** — ADJ-03, **resolved in phase 40**: the historical mismatch was a
  Python-side undercount, not an oracle disagreement. The Rust 5-term total and the oracle's 5-term
  `SumOfEvents` always agreed exactly, so this row carries no tolerance and is held to exact
  agreement like every other. See `docs/ADJUDICATIONS.md`.

The `geographic-concentration` row is deliberately kept exactly as it is: it disagreed once, for a
real reason (ADJ-01, above), and now serves as the regression guard against that defect returning.
Neither side should be adjusted to force agreement.

#### Table 2 — coverage / exclusions

Columns: `Probe | Category | Reason`. Every excluded probe is named with a **specific reason
citing the source of its declared population** — never a blanket "Tier-2" label.

`ExclusionCategory` has four variants:

| Category | Meaning |
|----------|---------|
| `IdentityLinksSourced` | Population comes from the `identity_links` table, not `graph_events` |
| `VerdictThreshold` | Population is a hardcoded `Verdict::classify(1, 1, …)` threshold, not a real count |
| `SemanticMismatch` | Population counts something structurally different from an entity/edge count (a term count, a scalar marker field) |
| `MarkerArithmetic` | Population is arithmetic on scalar fields of a single marker record, not a count of any set |

The **12 excluded probes** (declared in `EXCLUDED_PROBES`, same module as `CHECKED_PROBES`):

| Probe | Category |
|-------|----------|
| `detector-readiness` | `SemanticMismatch` — population, when reported, is `term_counts.len()`, the number of distinct motif terms tracked |
| `identity-link-registry` | `IdentityLinksSourced` — `scan_identity_links(db, None, None)` |
| `osint-confirmed-intel` | `IdentityLinksSourced` — `scan_identity_links(db, None, None)` filtered to confirmed links |
| `osint-analyst-queue` | `IdentityLinksSourced` — `scan_identity_links(db, Some("pending_review"), None)` |
| `psc-beneficial-ownership` | `IdentityLinksSourced` — `scan_identity_links(db, None, Some("ch_psc"))` |
| `phase20-ec-donations` | `IdentityLinksSourced` — `scan_identity_links(db, None, Some("electoral_commission"))` |
| `opencorporates-cross-jurisdiction` | `MarkerArithmetic` — `marker.candidates_found`, a scalar recorded by the adapter's own enrichment-run marker |
| `osint-disqualified-directors` | `VerdictThreshold` — hardcoded `Verdict::classify(1, 1, matches, hit_threshold, "")` |
| `donation-contract-correlation` | `IdentityLinksSourced` — `ec_links.len()`, sourced from `identity_links` |
| `osint-risk-edges` | `IdentityLinksSourced` — combines a `graph_events` scan with an `identity_links` scan; not a single independently recomputable count |
| `osint-evasion-alerts` | `VerdictThreshold` — hardcoded `Verdict::classify(1, 1, 1, 1, "")` |
| `land-flip-candidates` | `MarkerArithmetic` — arithmetic on two scalar fields of the `land_flip_runs:current` marker; the only genuinely independent check available is already enforced in-probe |

`land-flip-candidates` is **not** excluded for routing through `persistence::scan` — it does not.
It is excluded because its population is marker arithmetic with no set to group over, and because
an oracle re-reading the same two scalars would agree by construction and detect nothing new.

> **The coverage invariant is enforced at runtime, on both commands.** The check that
> `CHECKED_PROBES` and `EXCLUDED_PROBES` partition the live battery's own probe list — no gap, no
> overlap, in both directions — lives in one shared function,
> `population_source::verify_registered_probes_covered`. `probe-oracle`'s `compare()` calls it
> before rendering, and `probe::run::execute()` calls it on every ordinary `probe` run too, so
> battery drift fails loudly even if `probe-oracle` is never invoked. These were `debug_assert!`s —
> compiled out of release, since the workspace declares no `[profile.release]` — until finding
> **CR-02** was fixed on 2026-09-01. The count is taken against the battery run's own length, never
> a hardcoded 22, and a non-DB unit test reconciles both tables against
> `probe::run::registered_probe_names()`, so drift also fails at `cargo test`.

### Library API — `urban_integrity_core::persistence::oracle`

The counting primitives `probe-oracle` is built on, in `crates/core/src/persistence/oracle.rs`
(registered as `pub mod oracle;` in `crates/core/src/persistence/mod.rs`). They are a public Rust
API, not a network surface — nothing in `crates/api` exposes them.

```rust
pub async fn count_events(
    db: &Surreal<Any>, kind: &str, markers: &[&str],
) -> anyhow::Result<u64>

pub async fn count_distinct_entities(
    db: &Surreal<Any>, kind: &str, type_marker: &str, entity_id_prefix: Option<&str>,
) -> anyhow::Result<u64>

pub async fn count_distinct_edge_field(
    db: &Surreal<Any>, type_marker: &str, field: EdgeField,
) -> anyhow::Result<u64>

pub enum EdgeField { Src, Dst }
```

| Function | Query it issues | Notes |
|----------|-----------------|-------|
| `count_events` | `SELECT count() FROM graph_events WHERE kind = $kind [AND payload.weight_json CONTAINS $mN …] GROUP ALL` | One row per event. `markers` may be empty for a bare kind-only count. Every marker is bound individually as `$m{i}` — never interpolated into the SQL string — so a marker value cannot widen or narrow the query shape at runtime |
| `count_distinct_entities` | `SELECT count() FROM (SELECT payload.entity_id AS entity_id FROM graph_events WHERE … GROUP BY entity_id) GROUP ALL` | The dedup-by-entity primitive: a re-ingested entity contributes exactly once. `entity_id_prefix`, when supplied, adds `string::starts_with(payload.entity_id, $prefix)` |
| `count_distinct_edge_field` | `SELECT count() FROM (SELECT payload.src_id\|payload.dst_id AS v FROM graph_events WHERE kind = 'EdgeAdded' AND … GROUP BY v) GROUP ALL` | Always scoped to `kind = 'EdgeAdded'` — there is no `kind` parameter to get wrong. Groups by one endpoint only and never inspects the other. The field path is chosen at compile time from `EdgeField`; only `type_marker` crosses the input boundary, and it is always `.bind()`-parameterised |

All three return `0` rather than erroring when the grouped result set is empty, and every failure
is wrapped with an `oracle::<fn> query failed` context.

**Why a second implementation exists at all.** This module must never route through
`persistence::scan`, the codebase's load-bearing `graph_events` read path. If the oracle shared
`scan`'s implementation, a defect in it would be wrong in both places and agree with itself; kept
separate, the same defect surfaces as a disagreement in `probe-oracle`'s comparison table. That
independence (requirement ORC-06) is enforced structurally, not by convention:
`crates/core/tests/oracle_independence_guard.rs` fails if `oracle.rs` gains a `scan::` import or
call. Correctness is proved by `crates/core/tests/oracle_shapes.rs`, whose DB-gated tests include
a cross-check of the `GROUP BY` result against `scan_latest_entities_by_type`'s own `.len()` on a
controlled fixture — the two reaching the same number by entirely different mechanisms.

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
> or a materially larger graph. See the phase 38 code review.

### `verify` — Pre-signature Risk Audit (Phase 14)

Run a structured risk audit for a Companies House entity without re-triggering the
detector. Reads from existing SurrealDB alert records and PSC/disqualification data,
assembles a `VerifyVerdict`, and renders a coloured summary table or JSON.

```
urban-integrity-cli verify [OPTIONS]

OPTIONS:
    --company-number <CH>    Companies House registration number (required)
                             2–10 alphanumeric characters (e.g. 12345678, SC123456)
    --output <FORMAT>        table | json  (no default — auto-detected:
                             table on a TTY, json when piped)
    --summarize              Request a one-line Gemma4 summary via Ollama
                             (graceful WARN if unavailable)
```

**Exit codes:**
- `0` — entity not found, or risk level is LOW or MEDIUM
- `1` — risk level is HIGH or CRITICAL (scripting-friendly threshold)

`--output md` and `--output pdf` are not supported — use `report --entity` for document output.

**Risk dimensions reported:**

| Dimension | Description |
|-----------|-------------|
| Entity status | Active, Dissolved, or In Liquidation with date |
| P_c score | Composite corruption probability (0.0–1.0, percentile shown) |
| Active motifs | Count and types of motif alerts referencing this entity |
| Dissolved relatives | Count of dissolved companies sharing directors or addresses |
| Connected officers | Count of officers linked via the HIN graph |
| Disqualified director | Whether any connected officer holds an active CDDA 1986 ban |
| Beneficial owners (PSC) | Persons with Significant Control names from Companies House PSC data |
| Offshore link | Whether any PSC or registered address is in an offshore jurisdiction |
| Sanctions match | Whether any connected person appears on a sanctions list |

**Active-evasion override:** If a disqualified director is detected AND an active
`disqualified_director_evasion` motif alert references a linked officer, the risk level is
forced to **CRITICAL** regardless of the P_c score. Exit code is still `1`.

**Examples:**

```bash
# Audit a company by CH number (table output, default)
urban-integrity-cli verify --company-number 12345678

# JSON output for scripting
urban-integrity-cli verify --company-number 12345678 --output json

# With Ollama one-liner summary
urban-integrity-cli verify --company-number 12345678 --summarize

# Use in a CI pre-award check; non-zero exit = HIGH/CRITICAL
urban-integrity-cli verify --company-number 12345678 && echo "Passed"

# Extract risk level from JSON
urban-integrity-cli verify --company-number 12345678 --output json | jq '.risk_level'
```

### `enrich` — OSINT Enrichment (Phase 17–21)

Run OSINT enrichment adapters on demand against configured external sources. This is the
same enrichment logic that runs automatically in the background when `serve` is active,
exposed as a one-shot CLI command for manual backfill or debugging.

```
urban-integrity-cli enrich [OPTIONS]

OPTIONS:
    --source <SOURCE>    Adapter to run, or 'all' for all enabled adapters [default: all]
```

**Available adapters** (source name = value passed to `--source`):

| Source name | Description | Requires |
|-------------|-------------|----------|
| `ch_psc` | Persons with Significant Control (beneficial ownership) from Companies House | `URBAN__ENRICHER__CH__API_KEY` |
| `ch_disqualified` | Disqualified Directors register from Companies House | `URBAN__ENRICHER__CH__API_KEY` |
| `land_registry_ccod` | Commercial and Corporate Ownership Data (UK company-owned property) from Land Registry | `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` |
| `land_registry_ocod` | Overseas Companies Ownership Data (overseas-owned UK property) from Land Registry | HMLR key (`URBAN__ENRICHER__OCOD__API_KEY`, falls back to the shared `LAND_REGISTRY` key when blank) |
| `london_gazette` | Insolvency notices from the London Gazette public REST API | None (public) |
| `charity_commission` | Charity Commission bulk open data (exact CH-number trustee join) | None (OGL open data) |
| `opensanctions` | PEP / sanctions screening via the external yente service | `non_commercial_confirmed = true` (CC-BY-NC gate) |
| `icij` | ICIJ Offshore Leaks bulk load (ODbL open data, isolated `icij_*` tables) | None (ODbL open data) |
| `electoral_commission` | Electoral Commission political donations (exact CH-number join) | None (OGL open data) |
| `council_registers` | Council Registers of Interest (Scrapling sidecar) | LPA codes configured + sidecar |
| `opencorporates` | OpenCorporates cross-jurisdiction (exact company-number join) | `oc_open_data_confirmed = true` + quota |
| `ppd` | Land Registry Price Paid Data — stages raw transactions into `ppd_transactions` (no graph mutation); feeds land-flip detection | None (OGL open data). The Rust default is `enabled = false`, but the committed `config.toml` sets `[enricher.ppd] enabled = true`, so it runs out of the box from this repo |
| `mutuals_register` | FCA Mutuals Public Register — ingests registered societies (co-operatives, community benefit societies, housing associations) as `Entity` nodes keyed `GB-MUT:{registration}` plus their registered address as a `Location` + `LocatedAt` edge. Creates no identity link of its own | None — one unauthenticated public CSV; enabled by default |
| `planning_identity` | Internal exact-match join: links `director:{slug}` planning-applicant nodes to Companies-House-sourced Entity/Officer nodes when normalised names are identical, writing both an `identity_links` audit row and a real `Directs` edge | None — purely an in-graph join; always enabled |

Adapters that are not enabled (missing credentials, unconfirmed licence gate, or
`enabled = false`) are still registered but report `is_enabled() == false` and are skipped
when `--source all` is used. Specifying a disabled adapter by name explicitly logs a warning
and returns empty stats.

**Ordering note:** `mutuals_register` is registered immediately before
`planning_identity`, so societies are present in the graph when the exact-name join runs in the
same `--source all` pass. Registered societies are corporate bodies that are neither companies
nor on the Charity Commission register (a society with charitable rules is an exempt charity), so
without the register adapter that whole class is invisible to every other adapter. A society
whose status is `Deregistered` maps to `EntityStatus::Dissolved`, which makes societies
first-class counterparties for Phoenix. `planning_identity` writes an abbreviated-name match
(e.g. `"G Jones"`) at confidence `0.80` — below the auto-link threshold, so it lands in the
review queue and produces no graph edge.

**Special source — `--source land-flip`:** This value does **not** run an adapter. It triggers
the post-adapter Pre-Permission Land Flip detector, which scans planning applications against
staged `ppd_transactions` rows and writes review-queue candidates to the `land_flip_candidates`
table (no identity links, no graph mutations). It prints a distinct line:
`Land-flip detection complete: N applications scanned, M candidates written to review queue.`
Review the resulting candidates with the `flips` command. Run `enrich --source ppd` first to
stage the Price Paid Data the detector reads.

> **Exact-match-only policy (2026-06-13):** In-app name-only fuzzy/phonetic matching was
> removed from every adapter. Identity links are now produced from exact identifiers only —
> e.g. exact Companies House company numbers (`exact_company_number`) or exact normalised
> councillor names (`councillor_exact_name`). The `opensanctions` adapter screens via the
> external **yente** service (it does not use the former in-app fuzzy machinery), and ICIJ
> tables are ingested without any name-only entity→offshore links.

**Output:**

```
Enrichment complete: N candidates, M auto-linked, X pending review.
```

- `candidates_found` — entities scanned across all adapters
- `auto_linked` — identity links written with confidence above the auto-link threshold
- `pending_review` — links written with `status = 'pending_review'` for human review

**Note:** In standalone CLI mode (Phase 21), `GraphMutation` events emitted by adapters are
drained to the `graph_events` WAL by a background drain task (`drain_to_wal`) — they are no
longer silently discarded. There is no hot graph in the CLI path; the hot graph is rebuilt
from the WAL on the next `serve` replay or `project` run. SurrealDB `identity_links` records
are always written regardless of mode.

**Examples:**

```bash
# Run all enabled adapters
urban-integrity-cli enrich

# Run only the PSC adapter
urban-integrity-cli enrich --source ch_psc

# Run the London Gazette adapter (no API key needed)
urban-integrity-cli enrich --source london_gazette

# Run Land Registry CCOD adapter
urban-integrity-cli enrich --source land_registry_ccod

# Run the Electoral Commission donations adapter
urban-integrity-cli enrich --source electoral_commission

# Stage Land Registry Price Paid Data into ppd_transactions (no graph mutation)
urban-integrity-cli enrich --source ppd

# Run the Pre-Permission Land Flip detector (writes land_flip_candidates review queue)
urban-integrity-cli enrich --source land-flip
```

### `links` — Identity Link Review (Phase 16)

Review and manage OSINT identity resolution links written by the enrichment engine.
Identity links connect entities in the HIN graph when two data sources refer to the
same real-world person or company. Links below the auto-link confidence threshold are
written as `pending_review` and surfaced here for human confirmation or rejection.

```
urban-integrity-cli links <SUBCOMMAND>
```

#### `links list` — List pending links

```
urban-integrity-cli links list
```

Lists **every** `pending_review` identity link — there is no cap. The read goes through
`scan::scan_identity_links`, whose paged scan orders by `id` and terminates only on an empty
page; confidence-descending is then applied in Rust over the complete set, as display ordering
only. The former `ORDER BY confidence DESC LIMIT 100` was not a truncation but a biased sample of
the highest-confidence rows — for a review queue whose whole purpose is surfacing the
low-confidence tail, that hid exactly what an analyst came for.

Columns: `source node | target node | type | confidence | adapter | timestamp`.

#### `links show <ID>` — Show evidence detail

```
urban-integrity-cli links show <ID>

ARGS:
    <ID>    Identity link ID (e.g. identity_links:abc123)
```

Prints all fields of the link record. The `evidence_json` field is pretty-printed.

Fields shown: `id`, `source_node_id`, `target_node_id`, `resolution_type`, `confidence`,
`adapter_name`, `status`, `created_at`, `evidence_json`.

#### `links confirm <ID>` — Confirm a link

```
urban-integrity-cli links confirm <ID>

ARGS:
    <ID>    Identity link ID
```

Updates the link status to `confirmed`, writes an `EdgeConfirmed` event to the `graph_events`
WAL, and prints:
`Confirmed: <source> -> <target> (<type>). Hot graph updated.`

> **Note:** `confirm` updates the SurrealDB `identity_links` record and appends an
> `EdgeConfirmed` row to the `graph_events` WAL. The confirmed edge is materialised into the
> in-memory hot graph on the next `serve` replay (the standalone CLI has no live hot graph).

#### `links reject <ID>` — Reject a link

```
urban-integrity-cli links reject <ID>

ARGS:
    <ID>    Identity link ID
```

Updates the link status to `rejected`, suppressing re-proposal by the enricher in future
runs. Prints: `Rejected: link <ID> suppressed from future enrichment runs.`

**Examples:**

```bash
# List all pending links (most confident first)
urban-integrity-cli links list

# Inspect evidence for a specific link
urban-integrity-cli links show identity_links:abc123

# Confirm the link (analyst has verified it)
urban-integrity-cli links confirm identity_links:abc123

# Reject the link (false positive — suppress it)
urban-integrity-cli links reject identity_links:abc123
```

### `flips` — Land-Flip Candidate Review (Phase 23)

Analyst review surface for **Pre-Permission Land Flip** candidates written by the land-flip
detector (`enrich --source land-flip`). Mirrors the `links` command but routes only to the
`land_flip_candidates` review queue. A confirmed candidate is a **review artifact, not a graph
edge** — `confirm` and `dismiss` never write to `graph_events` / the WAL and never touch the
hot graph.

```
urban-integrity-cli flips <SUBCOMMAND>
```

#### `flips list` — List pending candidates

```
urban-integrity-cli flips list
```

Lists **every** `pending_review` candidate — there is no cap. The read goes through
`scan::scan_land_flip_candidates` (paged, ordered by `id`, terminating on an empty page);
`created_at` descending is applied in Rust afterwards, as display ordering only. The former
`ORDER BY created_at DESC LIMIT 100` hid the oldest stale rows, which is precisely what this
queue exists to surface.

Columns: `id | tier | severity | planning_ref | ppd_date | price_gbp | corr_conf | date_source`.

#### `flips show <ID>` — Show gate evidence

```
urban-integrity-cli flips show <ID>

ARGS:
    <ID>    Land-flip candidate ID (e.g. land_flip_candidates:abc123)
```

Prints the candidate header plus three gate-evidence sections:
1. **gate-1 (address)** — `ppd_postcode`, `ppd_paon`, `ppd_street`, `ppd_transaction_id`, `ppd_price_gbp`
2. **gate-2 (temporal)** — `ppd_date_of_transfer`, `application_received_date`, `temporal_offset_days`, `date_source`
3. **gate-3 (corroboration)** — `corroborating_link_id`, `corroborating_link_confidence`, `corroborating_entity_type`

#### `flips confirm <ID>` — Confirm a candidate

```
urban-integrity-cli flips confirm <ID> [OPTIONS]

ARGS:
    <ID>    Land-flip candidate ID

OPTIONS:
    --note <NOTE>    Optional free-text analyst note (never logged at INFO)
```

Sets `status = confirmed`, records `reviewed_at`, and stores the optional note. Does **not**
write any WAL / graph_events row. Prints:
`Confirmed: candidate <ID> (status = confirmed). Review queue only — hot graph untouched.`

#### `flips dismiss <ID>` — Dismiss a candidate

```
urban-integrity-cli flips dismiss <ID> [OPTIONS]

ARGS:
    <ID>    Land-flip candidate ID

OPTIONS:
    --note <NOTE>    Optional free-text analyst note (never logged at INFO)
```

Sets `status = dismissed`, suppressing re-proposal in future detector runs.

**Examples:**

```bash
# List pending land-flip candidates
urban-integrity-cli flips list

# Inspect the three gate-evidence sections for one candidate
urban-integrity-cli flips show land_flip_candidates:abc123

# Confirm with an analyst note
urban-integrity-cli flips confirm land_flip_candidates:abc123 --note "buyer matches applicant director"

# Dismiss a false positive
urban-integrity-cli flips dismiss land_flip_candidates:abc123 --note "arm's-length sale, no link"
```

### `migrate` — Apply Relation Schema Constraints (Phase 21)

Apply `TYPE RELATION FROM ... TO ...` (IN/OUT) schema constraints to every relation table and
re-project the graph from `graph_events`. Requires a running SurrealDB instance.

The migration removes and cleanly redefines **12** relation tables with FROM/TO node-type
constraints, then re-projects `graph_events` into the newly constrained tables:

| Relation table | FROM → TO |
|----------------|-----------|
| `directs` | `entity \| officer` → `entity \| officer` |
| `located_at` | `entity \| contract` → `location` |
| `awarded_to` | `entity` → `contract` |
| `issued_by` | `entity` → `contract` |
| `represents` | `ward` → `contract` |
| `controls` | `psc` → `entity` |
| `trustee_of` | `entity` → `charity` |
| `owns_property` | `entity \| offshore_entity` → `property` |
| `offshore_link` | `entity` → `offshore_entity` |
| `donates_to` | `entity` → `political_actor` |
| `disqualified_from` | `officer` → `disqualification` |
| `declares_interest` | `political_actor` → `entity` |

> **The command's own help text and console output say "11 relation tables".** That string is
> stale: `MIGRATION_DDL` (`crates/cli/src/commands/migrate.rs`) issues 12 `REMOVE TABLE` and 12
> `DEFINE TABLE ... TYPE RELATION FROM` statements, and the module's own tests assert 12.
> `declares_interest` is the twelfth, added when declared interests were given their own relation.

```
urban-integrity-cli migrate [OPTIONS]

OPTIONS:
    --dry-run    Print the DDL statements without executing (safety preview)
```

**Prerequisite:** Run `urban-integrity-cli enrich` first so OSINT nodes are present in
`graph_events`. After the `REMOVE TABLE` step wipes the relation data, re-projection needs the
source nodes to exist or edges will be skipped.

**Execution steps (non-dry-run):**
1. Pre-flight projection to capture the `edges_skipped` baseline.
2. Apply the `REMOVE TABLE` + `DEFINE TABLE ... TYPE RELATION` DDL for all 12 tables.
3. Re-project `graph_events` into the new constrained tables.
4. Report the before/after `edges_skipped` delta. A positive delta emits a WARNING that OSINT
   nodes may be missing — re-run `enrich`.

Output: `Migration complete: edges_skipped before=N, after=M, delta=D`

```bash
# Preview the DDL without applying it
urban-integrity-cli migrate --dry-run

# Apply the migration and re-project
urban-integrity-cli enrich   # ensure OSINT nodes exist first
urban-integrity-cli migrate
```

### `purge-orphans` — Remove Orphan CCOD Property Records

Data-integrity cleanup for orphan records left behind by the pre-fix CCOD ingest. The first
full CCOD ingest wrote a `Property` node (`graph_events` `NodeAdded`) and an `identity_links`
row for every UK property record, but the corresponding `OwnsProperty` edge was silently
dropped whenever the owner `GB-COH:{company_number}` node was not tracked in the graph. The
result is orphan property nodes and identity links with no owning company. The CCOD adapter
emit loop is now pre-filtered so this cannot recur; this command removes the orphans the
crashed run already persisted.

```
urban-integrity-cli purge-orphans [OPTIONS]

OPTIONS:
    --apply    Actually delete the orphan records. Without this flag the command is a
               DRY RUN that only counts and reports what would be deleted.
```

**Orphan definition (ownership-based):** a `Property` `graph_events` `NodeAdded` row that is
**not** the destination of any `OwnsProperty` edge — i.e. nothing owns it. This is format-
independent and safe for both ownership models: CCOD property is owned by a `GB-COH:` company
and OCOD property by a `GB-ROE:` / `GB-OCOD:` overseas entity, and both legitimate kinds carry
an `OwnsProperty` edge, so neither is misclassified. (The earlier id-derived `GB-COH:{segment}`
definition wrongly flagged every OCOD overseas record — ~93k validated rows — as orphans.) An
empty owned-set aborts the run, refusing to treat the entire register as orphans.

Requires a running SurrealDB instance.

```bash
# Dry run — count and report orphans without deleting (default)
urban-integrity-cli purge-orphans

# Actually delete the orphan property nodes and identity links
urban-integrity-cli purge-orphans --apply
```

### `purge-ocod-owners` — Retire Superseded OCOD Owner Events

`graph_events` is append-only, so the superseded `GB-OCOD:` owner nodes and their ownership
edges must be retired **before** `enrich --source land_registry_ocod` re-emits owners under the
corrected key — otherwise the re-run adds correctly-keyed nodes beside the old fragmented ones
instead of merging them. `GB-ROE:` owners are never in scope.

```
urban-integrity-cli purge-ocod-owners [OPTIONS]

OPTIONS:
    --apply    Actually delete the superseded events. Without this flag the command is a
               DRY RUN that only counts and reports what would be deleted.
```

`--apply` scans, then requires the operator to type back a run-derived confirmation digest;
there is no `--yes` / `--force` escape and it refuses to run non-interactively. The delete works
from the exact ids the scan produced rather than a post-confirmation re-scan, and a removed-count
mismatch is a hard error. No API surface exposes this command.

### `analysis` — Analysis Lifecycle

Provision, list, and retire named analysis databases. Each analysis is an isolated
SurrealDB database scoped to a set of ONS wards, named content-addressably as
`an_<label-slug>_<hash8>`, where `hash8` is the first 8 hex chars of SHA-256 over the
sorted, de-duplicated ward codes — the same ward set always yields the same suffix, so
re-provisioning is idempotent and detectable. The registry of analyses lives in the
`analyses` table of the `_catalog` database.

Every CLI command accepts the global `--analysis <DB_NAME>` flag to operate on a named
analysis instead of the default `main` database. The flag is bridged to the
`URBAN__ANALYSIS` env var at startup so every command's config load resolves the same
analysis.

```
urban-integrity-cli analysis <SUBCOMMAND>

SUBCOMMANDS:
    create           Provision a new analysis (optionally cloning a baseline with --from)
    list             List provisioned analyses
    remove <DB>      Remove (retire) an analysis: dry-run by default, --apply to drop
```

#### `analysis create` — Provision a new analysis

```
urban-integrity-cli analysis create --label <LABEL> --wards-file <FILE> [OPTIONS]

OPTIONS:
    --label <LABEL>                 Human label, slugified into the db name
                                    (e.g. "Black Country 2026" → an_black_country_2026_<hash8>)
    --wards-file <FILE>             File of ONS GSS ward codes, one per line
                                    (1 uppercase letter + 8 digits, e.g. X05999001)
    --from <BASELINE>               Baseline database to clone from (e.g. uk_national).
                                    Omit to provision empty.
    --boundary-vintage <EDITION>    ONS boundary edition the ward codes are from
                                    (recorded in the registry) [default: unknown]
```

Validates all ward codes, computes the content-addressable db name, guards against
re-provisioning an existing analysis and against reserved names (`main`, `uk_national`),
optionally clones the baseline via `surreal export | import` (one-directional — the
source database is never written), applies the schema, creates the per-analysis
checkpoint directory (`data/analyses/<db_name>/checkpoints`), and flips the registry
row to `active`. Cloning carries `graph_events` plus cursor tables so streaming can
later resume against the clone.

#### `analysis list` — List provisioned analyses

```
urban-integrity-cli analysis list
```

Lists registry rows newest first. Columns:
`DB_NAME | LABEL | WARDS | BASELINE | STATUS | CREATED`.

#### `analysis remove <DB_NAME>` — Retire an analysis

```
urban-integrity-cli analysis remove <DB_NAME> [OPTIONS]

ARGS:
    <DB_NAME>    The analysis database name to remove (the registry db_name,
                 e.g. an_foo_deadbeef)

OPTIONS:
    --apply           Actually perform the removal. Without this flag the command is a
                      DRY RUN report (label, ward count, event count, created_at, and
                      Phoenix-window status).
    --decommission    Permit removing an analysis still inside the Phoenix detection
                      window — explicit intent to retire live evidence.
```

**Guardrails:**

- Reserved databases (`main`, `uk_national`) can **never** be removed, even with
  `--apply --decommission`.
- An analysis younger than the Phoenix detection window
  (`detector.phoenix_window_days` from config; fallback 180 days, matching the
  detector's own default) is refused unless `--decommission` is passed. The window is
  measured from the registry `created_at`, not from event timestamps.
- Crash-safe ordering: the registry row is marked `retiring`, the database is dropped,
  the checkpoint directory is deleted, then the row is stamped `retired`. An
  interrupted remove stays `retiring` so nobody trusts a half-dropped analysis.
- The dry-run report's event count is never best-effort-zeroed. If the count query fails or
  the analysis database is unreachable the line reads `UNAVAILABLE (<error>)`; a bare `0`
  there would have read as "empty, safe to remove" directly above the `--apply` instruction.

**Examples:**

```bash
# Provision an analysis for a ward list, cloned from the national baseline
urban-integrity-cli analysis create --label "Black Country 2026" \
  --wards-file wards.txt --from uk_national --boundary-vintage BGE-2025

# List all provisioned analyses
urban-integrity-cli analysis list

# Dry-run a removal (default)
urban-integrity-cli analysis remove an_black_country_2026_1a2b3c4d

# Actually retire it (outside the Phoenix window)
urban-integrity-cli analysis remove an_black_country_2026_1a2b3c4d --apply

# Retire an analysis still inside the detection window (deliberate)
urban-integrity-cli analysis remove an_black_country_2026_1a2b3c4d --apply --decommission

# Run any other command against a named analysis
urban-integrity-cli --analysis an_black_country_2026_1a2b3c4d status
```

---

## SurrealDB Native Tables

After running `urban-integrity-cli project`, the following native SurrealDB tables are populated
for direct SurrealQL queries and Surrealist visual exploration.

### Node tables

| Table | Corresponds to |
|-------|----------------|
| `entity` | Companies and planning applicants |
| `officer` | Company officers / directors |
| `location` | Registered and operational addresses |
| `contract` | Contracts Finder and planning application records |
| `ward` | Ward nodes (ONS ward codes) |
| `political_actor` | Councillors and elected officials |
| `psc` | Persons with Significant Control (Companies House PSC data) |
| `charity` | Charity Commission registered charities |
| `property` | Land Registry title records (CCOD/OCOD) |
| `offshore_entity` | ICIJ Offshore Leaks entities |
| `disqualification` | CDDA 1986 disqualification records (their own node kind, not an attribute of an officer) |

### Relation tables

The `migrate` command constrains all 12 relation tables with `TYPE RELATION FROM ... TO ...`.
The core five are:

| Table | Meaning |
|-------|---------|
| `directs` | Officer → Entity (directorship) |
| `located_at` | Entity → Location (address) |
| `awarded_to` | Entity → Contract (award) |
| `issued_by` | Entity → Contract (issuing authority) |
| `represents` | Ward → Contract (contract/application sited in ward) |

The remaining seven OSINT relation tables (`controls`, `trustee_of`, `owns_property`,
`offshore_link`, `donates_to`, `disqualified_from`, `declares_interest`) are also constrained by
`migrate` and populated as the corresponding enrichment adapters write edges. `project` writes
all 12: its `EdgeWeight` → table mapping (`crates/ingestor/src/projector.rs`) covers every
variant, and its `NodeWeight` mapping covers all 11 node tables above.

All node records carry an `entity_id` field matching the HIN `EntityId` (e.g. `GB-COH:12345678`,
`director:john-smith`). All relation records carry temporal `meta.created_at`,
`meta.effective_from`, and `meta.effective_to` fields.

### `identity_links` table

Written by the enrichment engine. Each row represents a proposed or confirmed identity
link between two HIN graph nodes.

| Field | Description |
|-------|-------------|
| `source_node_id` | HIN `entity_id` of the source node |
| `target_node_id` | HIN `entity_id` of the target node |
| `resolution_type` | How the link was resolved. Exact-identifier values only — e.g. `exact_company_number`, `councillor_exact_name`, `ch_psc`, `sanctions_screening`, `name+dob_full`, `name+year`, and `planning_applicant_exact_name` |
| `confidence` | Float 0.0–1.0; links at/above the auto-link threshold (0.85) are recorded as `auto_linked` |
| `source` | Source adapter: `ch_psc`, `ch_disqualified`, `land_registry_ccod`, `land_registry_ocod`, `london_gazette`, `charity_commission`, `opensanctions`, `icij`, `electoral_commission`, `council_registers`, `opencorporates`, `ppd`, and `planning_identity`. Note `links show` (`crates/cli/src/commands/links.rs`, the `show` field list) reads `adapter_name`/`created_at`, which are not columns on this table — those cells render as `-`. |
| `status` | `pending_review` \| `auto_linked` \| `confirmed` \| `rejected` \| `candidate` \| `sub_candidate` |
| `resolved_at` | ISO 8601 timestamp |
| `evidence_json` | Raw evidence blob from the adapter |

> **No fuzzy/phonetic `resolution_type` values exist.** In-app name-only fuzzy matching was
> removed (2026-06-13); legacy values such as `name_match`, `address_match`,
> `fuzzy_name_match_offshore`, and `cross_jurisdiction_name_match` are no longer written.
> OpenSanctions links (`sanctions_screening`) come from the external yente service, not in-app
> fuzzy matching.

### Example SurrealQL queries

```surql
-- Look up a company by its HIN EntityId
SELECT * FROM entity WHERE entity_id = "GB-COH:12345678";

-- Find all directors of a named company
SELECT <-directs<-officer AS directors FROM entity WHERE name = "Apex Build Ltd";

-- Find all contracts awarded to an entity
SELECT ->awarded_to->contract AS contracts FROM entity WHERE entity_id = "GB-COH:12345678";

-- Find the contracts and planning applications sited in a ward
SELECT ->represents->contract AS contracts FROM ward WHERE entity_id = "ward:X05999001";

-- List all pending identity links for analyst review
SELECT * FROM identity_links WHERE status = 'pending_review' ORDER BY confidence DESC;

-- Find all confirmed links produced by the PSC adapter
SELECT * FROM identity_links WHERE status = 'confirmed' AND source = 'ch_psc';
```

> **Note:** The `project` and `migrate` commands use a raw HTTP `/sql` path to insert relation
> records, bypassing a flatbuffers codec mismatch between the SurrealDB 3.0.x server and the
> 3.1.x SDK. This is transparent to the caller.
