<!-- generated-by: gsd-doc-writer -->
# Configuration Reference

<!-- GSD-MANAGED-DOC: configuration -->

All configuration is loaded from **one TOML file in the project root** — `config.local.toml` if it exists, otherwise `config.toml` — with environment variables layered on top. Environment variables override config file values using the prefix `URBAN__` with double-underscore section separators.

**Example:** `[ingest].ch_api_key` → `URBAN__INGEST__CH_API_KEY`

The loader is `AppConfig::load()` in `crates/core/src/config.rs`: it calls `dotenv::dotenv()`, picks the file with `AppConfig::config_file_in(".")`, then layers that file under `Environment::with_prefix("URBAN").separator("__")`.

### `config.toml` (example) and `config.local.toml` (yours)

| File | Committed? | What it is |
|------|-----------|------------|
| `config.toml` | yes | A **fictional, loadable example**. Database `an_example_00000000`, planning portals present only as commented-out examples with made-up `X06…`/`X05…` codes and `planning.example.invalid` URLs, placeholder `XX…` postcode districts with Price Paid Data disabled. Enough for the toy analysis (`scripts/toy-analysis.sh`), which takes its ward from the fixture, not from config. |
| `config.local.toml` | **no** (gitignored) | Your real configuration: your analysis database, your wards and portals, your postcode districts, your deprivation index. Start it as a copy: `cp config.toml config.local.toml`. |

**Precedence, lowest to highest:**

1. **The file** — `config.local.toml` when it exists as a regular file, otherwise `config.toml`. Exactly one is read.
2. **`URBAN__*` environment variables** (including any loaded from `.env`).
3. Command-specific flags that overwrite a loaded value (`--analysis`, `workbench --bind`).

> **`config.local.toml` replaces `config.toml`; the two are never merged.** That is deliberate. The `config` crate (0.14) deep-merges *tables* across sources and replaces *arrays* wholesale, so an overlay would not concatenate `[[ingest.planning_portals]]` — but any key your file omitted would fall through from the example, and a real analysis could quietly inherit an example database name, PPD setting or postcode allowlist. Reading one file closes that off, and makes "what config is this run using?" a question about one file. The cost is that `config.local.toml` must be complete — which it is if you start from a copy. `AppConfig::load_file(path)` loads a specific file (plus env) when you need to bypass the choice, e.g. tests that assert on the committed example.

Every script that parses the config file itself (`scripts/surreal_target.py`, `scripts/analyst_probe.py`, `scripts/export_parquet.sh`, `scripts/rebuild-anglesey.sh`) makes the same choice. `scripts/deploy-demo.sh` generates its own config for the demo host and deletes any `config.local.toml` there.

In the rest of this reference, "`config.toml`" means whichever of the two files is in effect; "the shipped `config.toml`" means the committed example.

`AppConfig` has eight fields. Four are **required** in `config.toml` — `[surreal]`, `[graph]`, `[ingest]`, `[detector]` — and omitting any of them is a hard parse error, not a fallback to its `Default` impl. Three carry `#[serde(default)]` and may be omitted entirely: `[enricher]`, `[jurisdiction]` and `[workbench]`. The eighth is an optional top-level scalar key, `analysis` (see "Analysis Selection" below), normally supplied per-invocation via the CLI's global `--analysis` flag rather than written into `config.toml`.

**Strictness differs per section.** `#[serde(deny_unknown_fields)]` is applied to the top-level `AppConfig` and to `[surreal]`, `[graph]`, `[detector]`, `[jurisdiction]`, `[workbench]` and `[enricher.ppd]` — an unrecognised key in any of those is a hard parse error. `[ingest]` and every other `[enricher.*]` sub-section deliberately omit it, so forward-compatible keys can be added without breaking older config files.

> **An unknown `URBAN__*` section makes `AppConfig::load()` FAIL — it is not a silent no-op.** The environment source nests on `__`, so `URBAN__DB__ENDPOINT` becomes the config key `db.endpoint`; `AppConfig` carries `#[serde(deny_unknown_fields)]`, so deserialisation rejects the unknown field `db` and **every** command exits at startup with a config error, including ones that never touch the setting you were trying to override. A misspelled section is therefore loud, and a misspelled *key* inside a strict section is loud too — but a misspelled key inside `[ingest]` or a non-strict `[enricher.*]` sub-section is silently ignored, because those sections deliberately allow unknown fields.
>
> **`URBAN__DB__ENDPOINT` is not a real key** — there is no `[db]` section anywhere in `AppConfig`. The database connection URL is `URBAN__SURREAL__WS_URL` (mapping to `[surreal].ws_url`).

> **Toolchain & pinned dependency versions:** The Rust toolchain is pinned to `1.96.0` in `rust-toolchain.toml` (single source of truth for local dev and CI — `rustup` auto-installs it with `clippy`/`rustfmt`). SurrealDB is pinned to `surrealdb = "=3.1.0-beta.2"` (features `protocol-ws`, `protocol-http`) in the workspace `Cargo.toml` — do not bump either without a coordinated migration.

> **No fuzzy/phonetic matching knobs:** All in-app fuzzy and phonetic name-matching was removed on 2026-06-13 (the `strsim` and `rphonetic` dependencies were dropped from `crates/enricher/Cargo.toml`). There are no similarity-threshold, phonetic, or fuzzy-match configuration keys in any `[enricher.*]` section — adapters link only on exact identifiers. Some adapter source files still contain stale doc comments referencing fuzzy matching; those code paths are inactive and expose no configuration.

---

## Analysis Selection — `--analysis` / `URBAN__ANALYSIS`

Every CLI command can be pointed at a named *analysis* — an isolated SurrealDB **database** provisioned by `urban-integrity-cli analysis create` and tracked in the `analyses` table of the `_catalog` database (under the configured namespace).

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `analysis` (top-level) | string? | `None` | Selected analysis id (the registry `db_name`, e.g. `an_example_00000000`). `None` or `"main"` → use `[surreal].database` verbatim, with no registry access. Any other id is resolved via the `_catalog` registry; unknown, `retired`, or `retiring` ids are a hard error. |

> **An analysis id is a DATABASE name, not a namespace.** This distinction has already produced a wrong query in practice. The namespace is fixed for the whole install (`[surreal].namespace`, shipped as `urban_integrity`); the analysis id varies per analysis and is what `use_db()` selects. `connect_to_db()` in `crates/cli/src/db.rs` calls `.use_ns(&config.surreal.namespace).use_db(db_name)` — namespace from config, database from the resolved analysis. When querying SurrealDB by hand over HTTP, send `NS: urban_integrity` and `DB: <analysis id>` (e.g. `an_example_00000000`). Putting the analysis id in the `NS` header selects an empty namespace that SurrealDB will happily create for you, and the query returns zero rows with no error. On a **write** over raw HTTP the same mistake surfaces as `Couldn't write to a read only transaction` — an error message that says nothing about namespaces and sends you looking for a permissions problem that does not exist. The `_catalog` registry is likewise another **database** under that same namespace.

Resolution happens in one choke point — `resolve_analysis_db()` in `crates/cli/src/db.rs`, called by `connect_db()` for every command. Three equivalent ways to select an analysis:

```bash
urban-integrity-cli --analysis <db_name> report --ward X05999001   # global CLI flag (any command)
URBAN__ANALYSIS=<db_name> urban-integrity-cli status               # env var directly
# or set `analysis = "<db_name>"` at the top level of config.toml (unusual)
```

The `--analysis` flag is bridged to `URBAN__ANALYSIS` in `main.rs` before `AppConfig::load()` runs, so both paths are identical. The Python forensic scripts honour the same selector — see "Python Forensic Scripts Configuration" below.

**Lifecycle commands:**

```bash
urban-integrity-cli analysis create --label "Black Country 2026" --wards-file wards.txt \
    [--from uk_national] [--boundary-vintage 2022]
urban-integrity-cli analysis list
urban-integrity-cli analysis remove <db_name> [--apply] [--decommission]
```

- `create` reads `--wards-file` as **one ONS GSS ward code per line** (blank lines skipped, every code validated), slugifies the label into a content-addressable db name (`an_<slug>_<hash8>`), optionally clones a baseline database via `surreal export | import` (read-only on the source), applies the schema, and registers the row as `active`. `--boundary-vintage` records which ONS boundary edition the ward codes come from (default `"unknown"`); it is stored in the registry and is not used for resolution. The clone step locates the `surreal` binary via the `SURREAL_BIN` env var, else `~/.surrealdb/surreal`, else `surreal` on `PATH`.
- `remove` is a dry-run report by default; `--apply` marks the registry row `retiring`, drops the database, deletes its checkpoint dir, then stamps `retired` (crash-safe ordering — an interrupted remove stays `retiring`). Removing an analysis still inside the Phoenix detection window (measured from the registry `created_at` against `[detector].phoenix_window_days`, falling back to 180 when unset) is refused unless `--decommission` is passed.

---

## Startup Jurisdiction Preflight

Before any subcommand dispatches, `main.rs` runs `jurisdiction_preflight()` (`crates/cli/src/preflight.rs`) against the loaded config. It establishes **which UK nation the analysis covers** and refuses to start when the configuration cannot be right for it.

The nation is **derived from the configured ward codes** — the ONS GSS prefix names it (`E…` England, `W…` Wales, `S…` Scotland, `N…` Northern Ireland, on a nine-character code: one nation letter and eight digits). `[jurisdiction]` exists only to override or relax that derivation.

**Two conditions are refusals, not warnings**, because each silently produces a plausible-looking analysis that is wrong:

1. **Wards spanning more than one nation.** No deprivation index covers two nations, and HM Land Registry covers England and Wales only, so one run would be scored on two incomparable scales and enriched from a registry right for only part of it. Set `[jurisdiction] allow_mixed_nations = true` to take that on deliberately.
2. **A deprivation index from another nation.** It loads cleanly, matches none of the configured wards, and leaves every alert at `m_env = 1.0` — indistinguishable in the payload from "these wards are not deprived". The error names both nations and the offending `imd_csv_path`.

A **missing or unreadable** index is *not* refused: the detector already degrades to unweighted scoring and logs it, and turning that into a hard stop would break every analysis that has no index yet.

The preflight also logs which registered sources have no data for the analysis nation. Those are refused at run time by the enricher engine and counted as `skipped_out_of_jurisdiction`, so their absence appears as a coverage gap rather than a zero result. Source coverage is derived from `build_default_adapters` (`engine::source_coverage()`), so a new adapter cannot declare coverage the preflight does not know about:

| Coverage | Sources |
|----------|---------|
| **England and Wales only** | `land_registry_ccod`, `land_registry_ocod`, `ppd`, `charity_commission` |
| **UK-wide** | `ch_psc`, `ch_disqualified`, `london_gazette`, `opensanctions`, `icij`, `electoral_commission`, `council_registers`, `opencorporates`, `mutuals_register`, `planning_identity` |

Registers of Scotland, Land & Property Services NI, OSCR and CCNI are not implemented — a Scottish or Northern Irish analysis has no property or charity registry source at all, and the preflight says so up front.

An analysis with **no** `[[ingest.planning_portals]]` entries has no derivable nation. Nothing is gated and every source is admitted; the preflight logs that the nation is unknown rather than implying coverage was checked.

---

## `[jurisdiction]` — Nation Scope

Optional (`#[serde(default)]`) and normally **absent** — the nation is derived from ward codes. Carries `#[serde(deny_unknown_fields)]`.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `nation` | string? | `None` | Explicit nation for an analysis whose scope is not expressed as wards. Accepted: `england`, `wales`, `scotland`, `northern ireland` (also `northern-ireland`, `northern_ireland`, `ni`). Case-insensitive; spaces, hyphens and underscores are interchangeable. If wards **are** configured and disagree with this value, the config is refused rather than silently resolved either way. |
| `allow_mixed_nations` | bool | `false` | Permit wards from more than one nation. Off by default: no deprivation index spans two nations, and the England-and-Wales registries would be right for only part of the corpus. Setting it `true` leaves the analysis nation **unknown**, which admits every source — the operator takes on the interpretation. |

```toml
# Normally omitted entirely. Present only to declare a nation for an analysis with
# no wards, or to deliberately allow a mixed-nation analysis.
[jurisdiction]
nation = "scotland"
allow_mixed_nations = false
```

The shipped `config.toml` carries this section **commented out**, with the reasoning inline. A declared `nation` does not license a mixed ward set: the mix is the problem, and picking one of them would hide it.

---

## `[surreal]` — Database Connection

| Key | Type | Shipped value | Description |
|-----|------|---------------|-------------|
| `ws_url` | string | `ws://localhost:8000` | SurrealDB connection URL. The CLI's `connect_to_db()` explicitly rewrites `ws://` → `http://` (and `wss://` → `https://`) before connecting, so all CLI commands talk to SurrealDB over HTTP regardless of the scheme configured here. Use the exact host/port of the running server (the `scripts/surreal-start.sh` launcher binds `[::]:8000` by default — see "Launching SurrealDB" below). |
| `namespace` | string | `urban_integrity` | SurrealDB **namespace**. Fixed for the whole install; it does not vary per analysis. |
| `database` | string | `an_example_00000000` | Default SurrealDB **database**, used when no `--analysis` is selected (or when `analysis = "main"`). The shipped value is a fictional analysis id; in `config.local.toml` set it to the id `analysis create` printed for your analysis (format `an_<label-slug>_<8 hex>`). |
| `username` | string? | `root` | DB username (optional in config — SurrealDB v3 rejects anonymous access in practice). |
| `password` | string? | `root` | DB password (optional in config — SurrealDB v3 rejects anonymous access in practice). |

`[surreal]` is a required section with `#[serde(deny_unknown_fields)]`. `ws_url`, `namespace`, and `database` are mandatory (no `Default` impl — the "Shipped value" column above shows what the repository's `config.toml` sets); `username`/`password` are `Option<String>` and may be omitted from `config.toml`.

```toml
[surreal]
ws_url = "ws://localhost:8000"
namespace = "urban_integrity"   # namespace — one per install
database = "main"               # database — one per analysis; shipped config.toml sets "an_example_00000000"
username = "root"
password = "root"
```

> **`namespace` and `database` are not interchangeable.** Every analysis lives in its own **database** inside the single `urban_integrity` **namespace**. An id such as `an_example_00000000` is a database name. Sending it as the namespace on a raw HTTP query silently selects an empty namespace and returns no rows — and on a write, the misleading `Couldn't write to a read only transaction`.

> **Credentials required at runtime:** SurrealDB v3 rejects unauthenticated connections by default. Always set `username`/`password` in `config.toml` or via `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` environment variables — never commit real production credentials to `config.toml`.

**Session token expiry & re-auth:** The SurrealDB HTTP session token expires after roughly one hour. Two re-auth paths use the `[surreal]` credentials:

- `reauth_db()` in `crates/cli/src/db.rs` re-signs in a shared connection when WAL writes start returning `401 Unauthorized`. If the token has *genuinely* expired, an in-place `signin` cannot succeed either — SurrealDB attaches the stale bearer token to every request it sends, including `signin`, `invalidate` and `authenticate` — so `reauth_db()` falls back to opening a brand-new connection and returns it.
- The `enrich` command spawns a background re-auth task that fires every **50 minutes** (`50 * 60` seconds — deliberately inside the ~1 hour token TTL) and refreshes **both** the `EnricherEngine`'s own session (`EnricherEngine::reauth()`) and the independent WAL drain session — and re-selects the **resolved analysis database**, not the config default, so a long `--analysis` run cannot silently switch back to `main` at the first refresh. If either credential is absent from `SurrealConfig`, re-auth is a no-op. Ensure both values are set when running long enrichment jobs — a run longer than the token TTL otherwise loses every mutation drained after expiry.

**HTTP everywhere in the CLI:** `connect_to_db()` (shared by every CLI command, including `serve` and analysis provisioning) rewrites `ws://`/`wss://` to `http://`/`https://` and applies `SCHEMA_DEF` idempotently on connect. The `project` command and `export-graphml --source events` each construct an additional raw HTTP SQL client with the same rewrite (to bypass the SurrealDB 3.0.x server / 3.1.x SDK flatbuffers codec mismatch on relation records and on large result sets). Either way, the `ws_url` value you set is the only URL config needed.

### `export-graphml` — command-specific flags

`export-graphml` has **no configuration keys of its own**. It loads the standard `[surreal]` block via `AppConfig::load()`; everything else is CLI flags, defined by `ExportGraphmlArgs` in `crates/cli/src/commands/export_graphml.rs`.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-o`, `--output <PATH>` | path | `graph.graphml` | Output file path. |
| `--source <native\|events>` | enum | **`events`** | Where the graph is read from. `events` reconstructs it from the `graph_events` WAL (`NodeAdded` + `EdgeAdded` rows, paged 20,000 at a time to exhaustion) over the raw HTTP `/sql` endpoint, with no projection step. `native` reads the projected node/relation tables through the SurrealDB SDK and requires `urban-integrity-cli project` to have run first — it exits with `No nodes found in native tables` if it has not. |
| `--node-type <TYPE>` | string, repeatable | — | Keep only nodes of these types (case-insensitive), e.g. `--node-type Officer --node-type Entity`. Edges whose endpoints were filtered away are dropped with them. |
| `--edge-type <TYPE>` | string, repeatable | — | Keep only edges of these types (case-insensitive), e.g. `--edge-type Directs`. Nodes are untouched by an edge-only filter, so endpoints survive as isolates unless `--drop-isolated` is also passed. |
| `--drop-isolated` | flag | `false` | After filtering, drop every node left with no edges. Without it a type-filtered export is mostly unconnected dots in Gephi. |
| `--reachable-from-wards [CODE…]` | string list, optional values | absent → no reachability filter | Restrict the export to the weakly-connected component reachable from a set of ward seed nodes. Three states: **absent** → no filtering; **bare flag** → seeds fall back to every `ward_code` and every present `lpa_code` under `[[ingest.planning_portals]]` in `config.toml`; **with codes** (`--reachable-from-wards X05999001,X05999002`) → those codes override config entirely and are never merged with it. Applied strictly *before* `--node-type` / `--edge-type`. |

> **The default is `events`, not `native` — changed 2026-08-25 (commit `722b568`).** `native` goes through the SurrealDB SDK, whose flatbuffers codec fails to decode typed RecordId fields on large result sets: measured 2026-08-25, a 358k-node analysis errors outright with `Failed to deserialize flatbuffers payload`. The `events` path reads raw HTTP `/sql`, where every id arrives as a plain JSON string, and has no such ceiling. `native` is retained for small graphs and for exporting exactly what `project` produced. The default is pinned by a unit test (`default_source_is_events_not_native`) because it is a one-word edit away from silently regressing.

> **An unknown `--node-type` / `--edge-type` is a hard error, never an empty export.** `resolve_types()` fails naming both the unrecognised value and every type actually present in the graph. A typo such as `--node-type Offcier` would otherwise produce an empty GraphML indistinguishable from "this analysis has no officers" — the zero-output ambiguity this codebase has been bitten by before. A *valid* filter that legitimately matches nothing is still allowed to return an empty graph; the two cases are distinguishable precisely because the type name resolved.

> **`--source events` does not honour `--analysis`.** The events path is the one database reader in the CLI that bypasses `resolve_analysis_db()`: `load_from_events()` builds its own `reqwest` client straight from `config.surreal.{ws_url, namespace, database, username, password}` and sends `[surreal].database` as the `surreal-db` header verbatim. Only the `native` branch goes through `connect_db()`, which resolves the selector. Because `events` is now the default, `urban-integrity-cli --analysis <id> export-graphml` exports whatever `[surreal].database` names — not `<id>` — and no log line says so. Point `[surreal].database` at the analysis you mean, or pass `--source native`. Whatever `[surreal].database` names in the config file in effect is what gets exported; any other analysis is not. Both wrapper scripts — `scripts/export_graphml.sh` and `scripts/export_parquet.sh` — hardcode `--source events` and pass no `--analysis`, so they too export whatever `[surreal].database` names.

> **`--reachable-from-wards` is the only `export-graphml` flag that reads `config.toml` beyond `[surreal]`.** Its bare form resolves seeds through `effective_ward_codes()`, which collects every `ward_code` plus every present `lpa_code` across `[[ingest.planning_portals]]`, deduplicated and sorted (sorted only so the zero-seed error message is deterministic and diffable). Seeds are then matched by **exact equality** on `ward:{code}` against `Ward` nodes — not a substring test against arbitrary node ids. An LPA code on its own resolves nothing (there is no LPA node type) and that is fine alongside ward codes that do resolve; an effective set resolving to **zero** seeds is a hard error naming every code searched and every node type present, never a silent empty export. The traversal is deliberately **undirected**, so a node reachable only via the inbound direction of a directed edge is kept. Neither `scripts/export_graphml.sh` nor `scripts/export_parquet.sh` passes this flag, so both scripts export the unfiltered graph.

### `probe` / `probe-oracle` — command-specific flags

Neither command has configuration keys of its own. Both load `[surreal]` via `AppConfig::load()` and connect through `connect_db()`, so — unlike `export-graphml --source events` — both **do** honour `--analysis` / `URBAN__ANALYSIS`. Both are read-only against the database.

| Command | Flag | Default | Description |
|---------|------|---------|-------------|
| `probe` | `--output <PATH>` | `evidence-report.md` in the current working directory | Markdown evidence report path. The report is always written; `--output` only moves it. |
| `probe` | `--json <PATH>` | none written | Dumps the battery as JSON `Finding` records — all 22 of them, including any the gate skipped. Independent of `--output` — a `--json`-only invocation still writes the Markdown report. |
| `probe` | `--full-battery` | `false` — **gating is the default** | Runs the complete, ungated 22-probe battery. See the gating note below. |
| `probe-oracle` | `--output <PATH>` | none written | Writes a rendering of both tables (comparison, then coverage/exclusions) to disk. Without it, output goes to stdout only. |

Both commands create the parent directory of an `--output` / `--json` path if it does not exist. When `probe` is given both flags it attempts both writes before reporting either failure, so a run that has already done all its query work does not lose the artifact that succeeded; a write failure is still fatal.

> **`probe` gates on verdicts by default — `--full-battery` is the opt-out, not the other way round.** A bare `urban-integrity-cli probe` dispatches through `run_gated` (`crates/probe/src/graph.rs`) in topological order and evaluates a gate before each probe. Two rules apply: a probe with a *declared* dependency edge runs only if that named upstream returned `Hit` or `Weak` (the production battery declares no edges today — the former `address-cluster-exact` → `identity-integrity` edge was removed on 2026-09-03, and a test in `crates/probe/src/graph.rs` asserts none exist), and every **Tier-2** probe additionally gates on an OR across Tier-1 — if no Tier-1 probe returned `Hit` or `Weak`, the whole Tier-2 block is skipped. A gated-out probe is never silently absent: it still occupies its row in the table, the JSON and the Markdown report, carrying `Verdict::Skipped` with a cause naming the upstream probe or the tier, and its population provenance records `probe was gated out and never ran — no population to measure` instead of a fabricated figure. The dependency graph is validated on **every** run regardless of the flag, so a cyclic or drifted graph is a hard error before any probe touches the database.

`probe-oracle` runs the battery with `full_battery: true` set internally — it never gates, because a gated Tier-1 `Clear` would leave most Tier-2 rows `declared: None` and hollow out the very tables the command exists to print — then independently recomputes each checkable probe's population from `graph_events` through `crates/core/src/persistence/oracle.rs`, and prints a comparison table (probe, declared, oracle, allowed delta, agrees) followed by a coverage/exclusions table naming a specific reason per excluded probe. Of the 22 registered probes, 10 are independently recomputed and 12 are excluded with a stated category (`IdentityLinksSourced`, `VerdictThreshold`, `SemanticMismatch`, `MarkerArithmetic`).

> **The `--output` artifact is clean, byte-stable plain text — this was fixed, and an older caveat saying otherwise is stale.** `comfy_table` applies ANSI SGR styling whenever stdout is a TTY, so an earlier version reused the printed tables and wrote escape sequences into the file when run interactively (finding WR-03 in the phase 38 code review). The `--output` path now renders the artifact from *de-styled copies*: it rebuilds both tables and calls `force_no_tty()` on each before writing (`crates/cli/src/commands/probe_oracle.rs:263-274`). The artifact is therefore safe to feed a downstream parser and stable enough to diff between runs.

> **The 10-checked + 12-excluded coverage invariant is enforced at runtime.** It was `debug_assert!`-only — compiled out of release, since the workspace declares no `[profile.release]` — until finding CR-02 was fixed on 2026-09-01. `compare()` now returns an error, in both directions and counted against the battery run's own length rather than a hardcoded 22, instead of rendering a coverage statement that silently omits a probe.

### Launching SurrealDB (`scripts/surreal-start.sh`)

The database is not started by the Rust engine — launch it separately with `scripts/surreal-start.sh`. Where a systemd user session is available, the launcher runs SurrealDB as an isolated, memory-capped transient *user systemd service* (`urban-surreal`) via `systemd-run --user` under `user.slice`, so a database OOM cannot take down the editor or host (the cgroup OOM killer reaps SurrealDB alone). The unit also sets `MemorySwapMax=2G` and `OOMPolicy=stop`. Without a systemd user session (macOS, WSL without systemd, containers) it falls back to a detached background process with a PID file at `data/surreal.pid` and **no memory cap**, and says so. Both launch from the repository root. The launcher is idempotent — re-running is a no-op when a healthy instance is already up — and `scripts/surreal-start.sh --stop` stops whichever launcher started it.

> **Run the launcher from the repository root, and only from there — never from a git worktree.** `scripts/surreal-start.sh` derives `WORKSPACE_ROOT` from its own location (`SCRIPT_DIR/..`) and passes it as the unit's `WorkingDirectory`, while the store path is **relative** (`rocksdb://data/surreal-rocks.db`). Launched from a worktree, the unit therefore serves a brand-new *empty* RocksDB store under that worktree's own `data/` — which is gitignored, so nothing is left behind to notice. `/health` returns 200, `SCHEMA_DEF` applies cleanly, and `URBAN_DB_TESTS=1 cargo test --workspace` reports exactly the same passed/failed/ignored counts as a run against the real corpus, because the DB gate is a skip-by-return. **The only signal that dissents is duration** — the gated CLI test binary takes ~450s against the live store (measured 2026-09-16) and about a second against an empty one. This cost two invalid live-corpus verifications on 2026-09-03. A *fast* gated run is the thing to distrust, not a slow one.
>
> Before trusting any gated result, assert both of these:
>
> ```bash
> systemctl --user show urban-surreal -p WorkingDirectory   # must be the repository root
> curl -s -u root:root -H 'Accept: application/json' \
>      --data-raw 'INFO FOR ROOT;' http://localhost:8000/sql | grep urban_integrity
> ```
>
> The first must print the repo root; the second must list the `urban_integrity` namespace. If either fails, stop the unit and relaunch from the repo root — the launcher no-ops against an instance that already looks healthy, so the stop is required: `systemctl --user stop urban-surreal && scripts/surreal-start.sh`.

**Never run a bare `surreal start`.** A direct terminal launch inherits the calling shell's cgroup (e.g. VS Code's `app-code-*.scope`) and runs uncapped, so a database OOM triggers a *global* OOM that can take the editor down — and a `surrealkv://` path reused from shell history reintroduces the unbounded-memory storage engine that the RocksDB switch removed. (On a host with no systemd user session the launcher's background mode replaces a hand launch too — see [GETTING-STARTED.md step A4](GETTING-STARTED.md#a4-start-surrealdb).) `scripts/surreal-guard.sh` is an opt-in shell-function guard (source it from your `~/.bashrc`) that intercepts the `start` subcommand of `surreal`, refuses it, and routes to `scripts/surreal-start.sh` instead. All other `surreal` subcommands (`sql`, `import`, `export`, `version`, …) pass through untouched. To bypass the guard for a single intentional call, prefix it with `SURREAL_GUARD_BYPASS=1`. It finds the launcher in the checkout it was sourced from; set `URBAN_INTEGRITY_DIR` to point it at a different checkout.

**Storage backend:** As of 2026-06-17 the backend is **RocksDB** (`rocksdb://data/surreal-rocks.db`), which replaced the previous `surrealkv://` store. The switch was made because `surrealkv` held every touched key resident in RAM and OOM-killed SurrealDB during the CCOD bulk ingest; RocksDB uses an LSM tree with bounded memtables and a block cache that evicts to on-disk SST files.

The launcher and guard are tunable via environment variables (read by the shell scripts, **not** via the `URBAN__*` config namespace):

| Variable | Default | Description |
|----------|---------|-------------|
| `URBAN_SURREAL_DB_PATH` | `rocksdb://data/surreal-rocks.db` | Storage backend and path. Use `memory` for an ephemeral in-RAM store. |
| `URBAN_SURREAL_BIND` | `[::]:8000` | Address the server binds to (passed to `surreal start --bind`). The health check probes this port, and stale-process cleanup only touches servers bound to this address. |
| `URBAN_SURREAL_LAUNCHER` | `auto` | `systemd` (memory-capped user service), `background` (detached process, no memory cap, PID in `data/surreal.pid`), or `auto` (systemd when a user session answers, else background). |
| `URBAN_SURREAL_MEM_MAX` | `12G` | Hard cgroup `MemoryMax` cap (systemd launcher only). The cgroup OOM killer reaps SurrealDB alone if exceeded. Sized for a 32 GB host; lower it on a smaller machine. |
| `URBAN_SURREAL_MEM_HIGH` | `9G` | Soft cgroup `MemoryHigh` cap (systemd launcher only). The kernel throttles and reclaims here, nudging the storage engine to flush memtables before the hard wall. |
| `SURREAL_BIN` | `$HOME/.surrealdb/surreal` | Path to the `surreal` binary. Also read by `urban-integrity-cli analysis create --from` for the export/import clone step. |
| `URBAN_INTEGRITY_DIR` | — | Repository root, used by `scripts/surreal-guard.sh` when it cannot infer one. |
| `SURREAL_GUARD_BYPASS` | — | Set to `1` to let a single `surreal start` through the guard. |

The server is started with credentials `root`/`root` — match these in `[surreal].username`/`password` (or `URBAN__SURREAL__USERNAME`/`URBAN__SURREAL__PASSWORD`). Logs go to `data/surreal.log` (and, under systemd, `journalctl --user -u urban-surreal -f`).

```bash
scripts/surreal-start.sh                          # launch (idempotent — no-op if already healthy)
URBAN_SURREAL_MEM_MAX=8G scripts/surreal-start.sh # launch with an 8 GB hard cap
scripts/surreal-start.sh --stop                   # stop (either launcher)
URBAN_SURREAL_LAUNCHER=background scripts/surreal-start.sh  # force the no-systemd launcher
systemctl --user show urban-surreal -p MemoryCurrent  # watch RSS
```

`scripts/enrich.sh` starts the database through the same launcher (unless `--no-db` is passed) and stops the `urban-surreal` unit on exit.

---

## `[workbench]` — Analyst Workbench HTTP Server

Configures the HTTP listener of `urban-integrity-cli workbench`, the subcommand that replays an analysis's `graph_events` into a read-only in-memory graph and serves the browser workbench (embedded SPA assets plus `/health`, `/metrics`, `/graphql`, `/graphiql`, `/api/workbench-status` and `/api/analyses`). The section is optional (`#[serde(default)]` on `AppConfig.workbench`) and carries `#[serde(deny_unknown_fields)]`, so a misspelled key inside it is a hard parse error. Defined as `WorkbenchConfig` in `crates/core/src/config.rs`.

| Key | Type | Default | Shipped value | Description |
|-----|------|---------|---------------|-------------|
| `bind_addr` | string | `127.0.0.1:8090` | `127.0.0.1:8090` | Socket address the workbench HTTP server binds. Loopback by default. When the section is absent, `WorkbenchConfig::default()` supplies `127.0.0.1:8090`; when the section is present, `bind_addr` is required (there is no per-key default). |

```toml
[workbench]
bind_addr = "127.0.0.1:8090"
```

**Precedence, lowest to highest:** `config.toml` `[workbench].bind_addr` → `URBAN__WORKBENCH__BIND_ADDR` → the `workbench --bind <addr>` CLI flag (`crates/cli/src/commands/workbench.rs`, which overwrites `config.workbench.bind_addr` after `AppConfig::load()`).

**A taken port is a fail-loud error, never a silent re-pick.** `bind_listener()` in `crates/workbench/src/boot.rs` binds the socket **before** any SurrealDB round-trip, so a conflict surfaces in milliseconds rather than after a multi-minute replay. On `AddrInUse` it exits with an error naming the exact address and the three ways to change it (`[workbench] bind_addr` / `URBAN__WORKBENCH__BIND_ADDR` / `--bind`). The workbench never auto-selects a different port. Once bound, it prints `Workbench: http://<bind_addr>` and nothing else at startup; connections made during the replay queue in the kernel backlog rather than being refused.

**Command-line flags** (`WorkbenchArgs`):

| Flag | Default | Description |
|------|---------|-------------|
| `--bind <ADDR>` | none (use config/env) | Overrides `[workbench].bind_addr` for this invocation. |
| `--open` | `false` | Open the printed URL in the default browser (`xdg-open` / `open` / `cmd start`) once the listener is bound. Off by default — the workbench never opens a browser unless asked. |

**What else the workbench reads from `AppConfig`:**

- `[surreal]` and the analysis selector. It connects through `connect_db()` and resolves the target database with `resolve_analysis_db()` — the same path as `serve` — so `urban-integrity-cli --analysis <id> workbench` **does** target `<id>`. Its canonical-population read (`canonical_population_summary`, which reuses the `export-graphml` events loader) is given the **resolved** database name, unlike a plain `export-graphml --source events` run, which sends `[surreal].database` verbatim (see "`export-graphml` — command-specific flags").
- The `_catalog` analysis registry, for the analysis picker. Rows whose status is `retired` or `retiring` are not listed.
- Ward seeds for the population walk come from the target analysis's own `_catalog` row (`ward_codes`, recorded at `analysis create`), **not** from `config.toml`. Only the `main` / uncatalogued path, which has no registry row, falls back to the `[[ingest.planning_portals]]` ward and LPA codes. An empty `graph_events` table yields a population of zero rather than a boot failure, and zero resolvable seeds is reported on the status page as "population unavailable" rather than as an error.
- `[enricher.opencorporates]` — `enabled`, `oc_open_data_confirmed`, and whether `api_key` is non-blank after trimming — via `OcProbeGates::from_config()` (`crates/probe/src/run.rs`), so the in-process probe battery gates the OpenCorporates probe exactly as the CLI `probe` subcommand does.

> **`serve`'s own listeners are not configurable.** `serve` starts its HTTP/GraphQL server with `ServerConfig::default()` (`crates/api/src/server.rs`): HTTP on `127.0.0.1:8080` and gRPC on the next port, `127.0.0.1:8081`. There is no `config.toml` key or `URBAN__*` variable for either; `[workbench].bind_addr` affects only the `workbench` subcommand. The defaults do not collide (8080/8081 vs 8090), so both can run at once.

---

## `[graph]` — Graph Engine

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `checkpoint_events_n` | u64 | `1000` | Events between automatic WAL checkpoints |
| `checkpoint_interval_secs` | u64 | `60` | Seconds between automatic checkpoints (whichever fires first) |
| `checkpoint_path` | string | `data/checkpoints` | Directory for checkpoint files |
| `wal_channel_capacity` | usize | `10000` | Bounded WAL crossbeam channel capacity (backpressure valve) |
| `wal_batch_size` | usize | `100` | **Accepted but never read.** Parsed into `GraphConfig` and required in `config.toml`, but nothing outside the config module and test fixtures consumes it: `serve` constructs `SurrealWriterConfig::default()` and `BatchController::new` hardcodes a starting size of `100`. Changing this value has no effect. |
| `wal_batch_window_ms` | u64 | `100` | **Accepted but never read.** The writer's drain window is a hardcoded literal — `drain_batch(&rx, batch_size, 100)` in `crates/core/src/persistence/surreal.rs`, commented "Window is fixed at 100ms for now". The shipped value coincides with the hardcoded one; changing it has no effect. |
| `shard_count` | usize | `64` | DashMap shard count for `node_map`/`edge_map` |
| `snapshot_every_n_events` | u64 | `1000` | Events between CSR snapshot bakes |
| `snapshot_interval_secs` | u64 | `30` | Seconds between CSR snapshot bakes (whichever fires first) |

`[graph]` carries `#[serde(deny_unknown_fields)]`; all nine keys are required in `config.toml` (the struct has a `Default` impl matching the values above, but it is not applied during file loading).

```toml
[graph]
checkpoint_events_n = 1000
checkpoint_interval_secs = 60
checkpoint_path = "data/checkpoints"
wal_channel_capacity = 10000
wal_batch_size = 100
wal_batch_window_ms = 100
shard_count = 64
snapshot_every_n_events = 1000
snapshot_interval_secs = 30
```

**AIMD Batching note:** The `BatchController` starts at a hardcoded `100` events and dynamically adjusts at runtime between 10 and 5000 events based on observed SurrealDB commit latency (target: 50ms). Those three bounds come from `SurrealWriterConfig::default()` in `crates/core/src/persistence/surreal.rs`, **not** from `[graph]` — `wal_batch_size` is neither the starting value nor read at all (see the two rows above).

---

## `[ingest]` — Data Sources

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `ch_api_key` | string? | `""` | Companies House **streaming** API key — **set via env** (see below). Used by the CH streaming ingestor (`stream.companieshouse.gov.uk`) under `serve`, and by nothing else — every `urban-integrity-cli backfill` subcommand that talks to Companies House calls the REST API and needs `[enricher.ch].api_key` instead. If absent or empty, CH streaming is disabled at startup. This key is distinct from `[enricher.ch].api_key` and the two are **not** interchangeable. |
| `firecrawl_api_key` | string? | `""` | Firecrawl API key — **set via env** (see below). If absent or empty, the Firecrawl fallback in `ingest-planning` is disabled. |
| `cf_poll_interval_secs` | u64 | `900` | Contracts Finder polling interval (seconds). Default: 15 min. CF polling always starts regardless of whether a CH key is set. |
| `scrapling_url` | string | `http://localhost` | Scrapling sidecar base URL (no trailing slash) |
| `scrapling_port` | u16 | `8765` | Scrapling sidecar port |
| `scrapling_timeout_secs` | u64 | `300` | HTTP client timeout for a **single** sidecar `/scrape` call. See the sizing note below — this must clear the slowest whole-ward re-list, not the average one. |
| `planning_portals` | array | `[]` | Planning portal entries — see `[[ingest.planning_portals]]` below. Also the **source of the analysis nation** (see "Startup Jurisdiction Preflight"). |
| `planning_poll_interval_secs` | u64 | `3600` | How often the background poller re-scrapes each configured ward. Default: 1 hr |

```toml
[ingest]
# Set via URBAN__INGEST__CH_API_KEY environment variable
ch_api_key = ""
# Set via URBAN__INGEST__FIRECRAWL_API_KEY environment variable
firecrawl_api_key = ""
cf_poll_interval_secs = 900
scrapling_url = "http://localhost"
scrapling_port = 8765
scrapling_timeout_secs = 300
planning_poll_interval_secs = 3600
```

> **Sizing `scrapling_timeout_secs`:** a full ward re-list is **one** request that scrapes every application in the ward, so the ceiling has to clear the largest ward rather than the average one. Measured live against an Arcus portal on 2026-07-27: the largest ward, 4,986 applications, took 157.6 s and the next largest, 3,996 applications, 132.3 s. The previous hardcoded 180 s left roughly 13% headroom over the worst case, and a ward duly timed out under concurrent load during a backfill run. 300 s restores real headroom. Raise it before adding a substantially larger LPA.

**Getting a Companies House streaming API key:**
1. Register at [developer.companieshouse.gov.uk](https://developer.companieshouse.gov.uk)
2. Create a new application and request access to the **Streaming API**
3. Export: `export URBAN__INGEST__CH_API_KEY="<your streaming key>"`

**`serve` command startup behaviour:**
- **CH streaming** is optional. If `URBAN__INGEST__CH_API_KEY` is not set (or is empty in `config.toml`), a `WARN` is logged and CH streaming is skipped — the engine continues normally without it.
- **Contracts Finder polling** always starts unconditionally (no API key required — it is a public endpoint).
- **Planning portal polling** starts only if at least one `[[ingest.planning_portals]]` entry is configured; otherwise `serve` logs "No planning_portals configured — planning poller disabled". Scrapling sidecar errors are logged as `WARN` and never panic the engine.

**`backfill` and the CH REST key:** `urban-integrity-cli backfill` has six subcommands — `ch`, `ward`, `planning`, `cessation`, `officers`, `psc-links`. Those that call the Companies House **REST** API (`ch`, `cessation`, `officers`) require `[enricher.ch].api_key` (`URBAN__ENRICHER__CH__API_KEY`) — **not** the streaming key in this section. If that key is absent or empty the command exits immediately, e.g.: `Companies House REST key not configured — set URBAN__ENRICHER__CH__API_KEY. Note this is the REST key, a different credential from URBAN__INGEST__CH_API_KEY (streaming), which 401s against the REST search endpoint.` `backfill ch` previously read `ingest.ch_api_key`, so every lookup 401'd and no company number could be attached to anything; changed 2026-07-28. `backfill ward` needs no API key — it resolves ward codes via the public `postcodes.io` API. `backfill psc-links` makes no CH API calls at all (it re-runs the PSC↔officer join over data already in the graph).

> **`--rate-limit` defaults to 2 requests/second** on `backfill ch`, `cessation` and `officers`. Companies House allows 600 requests per 5 minutes — 2/s — and throttles above that. The default was 5, which produced `CH API 429 rate limit` errors within the first minute of a live run on 2026-07-28; the same run at 2/s logged zero 429s over an hour. Raise it only if you know your key has a higher quota.

### Scrapling sidecar process configuration

`scrapling-sidecar/` is a **separate** FastAPI process, not part of the Cargo workspace and not a reader of `config.toml`. It is **optional**: if it is down, ingestion logs a `WARN` and continues. It is configured entirely through its own environment variables (no `URBAN__*` prefix), and is started with `scripts/sidecar-start.sh`.

| Variable | Default | Description |
|----------|---------|-------------|
| `SIDECAR_PORT` | `8765` | Port the sidecar binds. Must match `[ingest].scrapling_port`. Also accepted by `scripts/sidecar-start.sh`. |
| `SIDECAR_HOST` | `127.0.0.1` | Bind address. Loopback by default — `/scrape` takes an arbitrary URL and fetches it, so exposing it beyond loopback hands out an SSRF primitive. |
| `SIDECAR_ALLOWED_HOSTS` | `""` (unset) | Optional comma-separated allowlist of exact hostnames the sidecar may fetch. The baseline guard is default-deny on *destination class* — every private, loopback, link-local, multicast or otherwise reserved address is refused regardless of this setting. Set this to additionally pin specific portal hosts. |
| `FIRECRAWL_API_KEY` | — | Sidecar-side Firecrawl credential. **Distinct from `URBAN__INGEST__FIRECRAWL_API_KEY`** — the sidecar does not read the Rust config namespace. The Firecrawl path is disabled if the key is unset *or* `firecrawl-py` is not installed, but **only the missing-package case logs anything**: `sidecar.py` emits `WARN firecrawl-py not installed — Firecrawl fallback disabled` from its `except ImportError` branch, while an unset key with the package present sets `_firecrawl = None` silently. A sidecar with Firecrawl disabled for want of a key looks identical in the logs to one with it enabled. |
| `URBAN_SIDECAR_PYTHON` | `scrapling-sidecar/.venv/bin/python` | Interpreter used by `scripts/sidecar-start.sh` (falls back to `python3` on `PATH`). |

> **Redirect handling is per-request, not per-client.** Scrapling's `Fetcher(follow_redirects=False)` constructor kwarg is accepted and then ignored. The URL guard is only meaningful if `follow_redirects` is passed on each `.get()`/`.post()` call — otherwise a validated public host that 302s to `127.0.0.1:8000` bypasses the guard entirely.

---

## `[[ingest.planning_portals]]` — Planning Portal Ward Configuration

Each entry in the `[[ingest.planning_portals]]` array configures one Local Planning Authority (LPA) ward for scraping. The background poller (started by `urban-integrity-cli serve`) polls each ward at `planning_poll_interval_secs` intervals. Use `urban-integrity-cli ingest-planning --all` for on-demand scraping.

| Key | Type | Description |
|-----|------|-------------|
| `lpa_code` | string? | ONS local-authority (LPA) code — `E06…`/`E07…`/`E08…`/`E09…` (England), `W06…` (Wales), `S12…` (Scotland), `N09…` (NI). Optional — used for entity resolution across LPAs. |
| `ward_code` | string | ONS GSS ward code — `E05…`, `W05…`, `S13…` or `N08…`. Used as the graph node key, for alert filtering, and as the **input to the jurisdiction preflight**. A value that is not a nine-character GSS code (one nation letter, eight digits) is a startup error. |
| `portal_type` | string | Scraping strategy passed verbatim to the Scrapling sidecar. Recognised values: `"idox"`, `"arcus"`, `"wfs"`, `"planapi"`. Defaults to `"idox"` (covers ~69% of UK LPAs). |
| `url` | string | Full URL for the ward's planning search results page |

The shipped `config.toml` carries three entries **commented out**, using fictional codes and a reserved `.invalid` host. They stay commented because `X05…` is not a GSS nation prefix, so the jurisdiction preflight would refuse them as unrecognised and every CLI command would exit at startup. With no active entries the analysis has no derived nation (see "Startup Jurisdiction Preflight"), `serve` logs "No planning_portals configured — planning poller disabled", and `ingest-planning` exits saying no portals are configured.

```toml
planning_poll_interval_secs = 3600

# FICTIONAL — copy the shape into config.local.toml with your own codes and URLs.
[[ingest.planning_portals]]
lpa_code    = "X06999001"
ward_code   = "X05999001"
portal_type = "idox"
url         = "https://planning.example.invalid/online-applications/search.do?action=advanced&ward=X05999001"

[[ingest.planning_portals]]
lpa_code    = "X06999001"
ward_code   = "X05999002"
portal_type = "arcus"
url         = "https://planning.example.invalid/s/register?ward=Example%20North"
```

**Filling in real entries.** One entry per ward of the authority you are studying:

- **Codes.** Ward and local-authority codes are ONS GSS codes. Look them up in the ONS Open Geography Portal (geoportal.statistics.gov.uk — the "Ward to Local Authority District Lookup" for the boundary year you want) or the ONS Postcode Directory. Use the same boundary vintage throughout, and record it with `analysis create --boundary-vintage`.
- **URL and `portal_type`.** Find the council's online planning register from its planning web pages and note which system runs it. Idox PublicAccess (`/online-applications/` in the path) is the most common (`"idox"`). Arcus Global registers on Salesforce Experience Cloud carry the ward as a query parameter — `?ward=<ward name exactly as the register spells it>` — and reach the sidecar's `scrape_arcus()` strategy (`"arcus"`).
- **Older wards.** A register may still tag some older applications with wards from a previous boundary set. Adding those legacy ward codes as extra entries keeps those records from being dropped.
- **Encoding.** Ward names carrying spaces, non-ASCII or reserved characters must be percent-encoded in the URL (`%20` for a space, `%27` for an apostrophe, UTF-8 bytes such as `%C3%B4` for `ô`).

All wards in one analysis should share a nation prefix: the derived nation gates the deprivation index and refuses the England-and-Wales-only registries for a Scottish or Northern Irish configuration.

**Graceful degradation:** If a planning portal is unreachable, the poller logs a `WARN` and continues to the next ward — the engine never panics on portal errors.

**Entity keying:** Applicant names are normalised to a `director:{name-slug}` key. This ensures that the same applicant appearing across different LPAs resolves to a single graph node, enabling cross-authority Phoenix motif detection.

---

## `[detector]` — Detection Parameters

| Key | Type | Default | Shipped value | Description |
|-----|------|---------|---------------|-------------|
| `phoenix_window_days` | i64? | `180` | `180` | Days to look back when matching active entities to dissolved predecessors. Also used by `analysis remove` as the retention window guard. |
| `phoenix_min_value` | f64? | `10000.0` | `10000.0` | Minimum contract value (£) to trigger Phoenix Loop scoring |
| `imd_csv_path` | string? | `None` | `data/wimd_2019_wards.csv` (example value — point it at your own nation's file) | Path to a ward-level deprivation-index CSV. Must belong to the same nation as the configured wards, or startup is refused. IMD weighting is disabled if absent or unloadable. |

`[detector]` carries `#[serde(deny_unknown_fields)]`. All three keys are `Option`; the "Default" column shows the struct's `Default` impl.

```toml
[detector]
phoenix_window_days = 180
phoenix_min_value = 10000.0
imd_csv_path = "data/wimd_2019_wards.csv"
```

**`serve` honours this block.** `crates/cli/src/lib.rs::detector_config_from()` builds the detector's runtime settings from `[detector]`, falling back to `180` / `10_000.0` / `None` when an individual key is absent. Note this is a per-key default only: `AppConfig.detector` carries no `#[serde(default)]`, so omitting the whole `[detector]` block is a hard parse error (``missing field `detector` ``), not a fallback. This was previously three inline literals in `serve` (`phoenix_window_days: 730`, `phoenix_min_value: 10_000.0`, `imd_csv_path: None`), which meant the only process that actually runs detection ignored the entire section: Phoenix ran a 730-day window while config said 180, and IMD weighting was off outright, pinning `m_env` to 1.0 on every alert. Keep the wiring behind this named function so it stays testable.

### The deprivation index file

`data/*` is gitignored, so **no index arrives with a clone**. Regenerate it with `scripts/build_deprivation_ward_index.py` rather than hunting for it:

```bash
python3 scripts/build_deprivation_ward_index.py --nation wales --lad <W06… code>  # one Welsh authority
python3 scripts/build_deprivation_ward_index.py --nation wales --all              # every Welsh ward
python3 scripts/build_deprivation_ward_index.py --nation england --lad <E0…code> <E0…code>
python3 scripts/build_deprivation_ward_index.py --nation northern-ireland --all
```

> **`--all-wales` is not a flag** (argparse rejects it), and a bare `--nation wales` / `--nation england` exits with `error: --nation <n> needs the local authorities to cover: <remedy>.` — for Wales the remedy is `use --all for every Welsh ward, or --lad <codes> to scope it`. The invocations in `config.toml`'s `[detector]` comment block match the forms above.

`--nation` is required and takes `england`, `wales`, `scotland` or `northern-ireland`. For England, Wales and Scotland you must name the local authorities with `--lad` — an analysis is ward-scoped, and building everything would page the entire national postcode directory. `--all` is accepted only for `wales` and `northern-ireland`. `--out` overrides the default output path, which is per nation: `data/wimd_2019_wards.csv`, `data/imd_2019_wards.csv`, `data/simd_2020_wards.csv`, `data/nimdm_2017_wards.csv`.

Until an index exists, `serve` logs `IMD index load failed` and alerts carry `m_env = 1.0` with a null `imd_rank` — un-weighted, but stated rather than silent.

**Where this setting is consumed.** All `P_c` scoring lives in `crates/detector/src/scoring/` — `imd.rs` loads and indexes the file, `formula.rs` applies it. The weighting is a **headroom uplift, not a multiplier**: `p_final = p_base + (1 - p_base) * IMD_WEIGHT * deprivation` with `IMD_WEIGHT = 0.3`, and the `m_env` reported on the alert is `p_final / p_base` after the fact rather than an input. Nothing routes through the `crates/stats` crate — it is still an empty `cargo new` template (one `add()` function and its test) and contains no scoring code, so do not look for the deprivation loader there.

**File format.** The loader is `ImdIndex::load` in `crates/detector/src/scoring/imd.rs`. It reads a headed CSV and requires two columns by exact name — `Ward Code` and `IMD Rank` — plus one strongly recommended optional column, `IMD Denominator`. The builder emits seven columns; the extra ones are audit-only and ignored:

```csv
Ward Code,Ward Name,IMD Rank,IMD Denominator,LSOA Count,Rank Min,Rank Max
X05999001,Example North,1573,1909,6,1300,1820
X05999002,Example South,1127,1909,2,954,1300
```

> **`IMD Denominator` is the load-bearing column.** `m_env` normalises against `max_rank`, and without a declared denominator the loader infers it from the largest rank present in the file. That is correct only for a whole-index file. A regional extract silently rescales itself: an index of one council's 14 wards, whose ranks live in Wales' 1..1909 space, would normalise against its own worst ward and give the least-deprived ward the maximum weighting — asserting it is the least deprived place in Wales. When the column is present it wins, and two guards come with it: a rank above the declared denominator is rejected (it would make the numerator negative and de-weight the alert), and rows disagreeing about the denominator are rejected (a file splicing England's 32,844-LSOA ranks onto Wales' 1,909 has no common scale). `ImdIndex.denominator_declared` records which path was taken, so a caller can say the scale was inferred rather than presenting it as declared.

> **One nation per file.** The UK publishes four deprivation indices and they are not comparable: England IMD 2019 (32,844 LSOAs), Wales WIMD 2019 (1,909 LSOAs), Scotland SIMD 2020 (6,976 data zones), Northern Ireland NIMDM 2017 (462 wards). All four rank 1 = most deprived, but a rank of 500 is the 26th percentile in Wales and the 1.5th in England. An index from the wrong nation is **refused at startup** by the jurisdiction preflight, not warned about mid-run — it would load cleanly, match zero wards, and leave the weighting inert while appearing correctly configured.

> **Ward-level deprivation is a coarse proxy, and Scotland is unusable at this resolution.** Measured across all four builds (2026-08-16): Northern Ireland 1 small area per ward (NISRA ranks wards directly — exact), England ~2 areas/ward (30% rank spread), Wales ~4 areas/ward (35%), Scotland ~42 areas/ward (**88%** spread). A Scottish ward's median rank summarises data zones spanning most of the national distribution — arithmetically fine, substantively meaningless. The builder aggregates by **median**, not mean, because a deprivation rank is ordinal; that is a deliberate, recorded deviation from MHCLG's official rank-of-average-score method for larger areas.

<!-- VERIFY: publication URLs and current editions of the four UK deprivation indices — external gov.uk / gov.wales / gov.scot / NISRA datasets, not derivable from the repository -->

---

## `[enricher]` — Enrichment Engine

Controls the background OSINT enrichment process. The `EnricherEngine` **sleeps first, then runs** each registered adapter in sequence, repeating on that cycle. The ordering is deliberate — `crates/enricher/src/engine.rs` records it as "D-04: sleeps BEFORE the first run — avoids hammering external sources on restart" — but note the consequence at the default `enrich_interval_secs = 86400`: a freshly started `serve` performs **no enrichment for its first 24 hours**. Use `urban-integrity-cli enrich` directly if you need a pass immediately after startup. The whole `[enricher]` section is optional in `config.toml` — if absent, `AppConfig` applies `EnricherConfig::default()` (every adapter sub-section also defaults). Each adapter sub-section is likewise optional and falls back to its own `Default`.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enrich_interval_secs` | u64 | `86400` | Seconds to sleep between enrichment adapter runs. Default: 24 hours, aligned with the Companies House bulk data export cadence. |

```toml
[enricher]
enrich_interval_secs = 86400
```

**Tuning guidance:** Lowering `enrich_interval_secs` increases enrichment frequency at the cost of more OSINT API calls. Values below 3600 (1 hour) are not recommended for production — external OSINT sources may rate-limit or block the engine.

**Environment variable override:** `URBAN__ENRICHER__ENRICH_INTERVAL_SECS` — follows the standard `URBAN__<SECTION>__<KEY>` pattern.

**Analysis-aware CLI runs:** `urban-integrity-cli enrich [--source <adapter>|all] [--analysis <id>]` connects to the resolved analysis database and re-authenticates both the engine session and the WAL drain session every 50 minutes against that same resolved database (see "Session token expiry & re-auth" under `[surreal]`).

**Jurisdiction gating:** an adapter whose coverage does not include the analysis nation is not run at all — it is counted as `skipped_out_of_jurisdiction` in `EnrichmentStats` rather than reported as a zero result. This applies to both the background loop and `enrich --source`. See "Startup Jurisdiction Preflight" for the coverage table.

### Adapter source names

`--source` takes one of the adapter names below (or `all`, or `land-flip` for the post-adapter land-flip correlation pass). These are the `EnricherAdapter::name()` values and the same strings `scripts/enrich.sh --source` accepts:

`ch_psc` · `ch_disqualified` · `land_registry_ccod` · `land_registry_ocod` · `london_gazette` · `charity_commission` · `opensanctions` · `icij` · `electoral_commission` · `council_registers` · `opencorporates` · `ppd` · `mutuals_register` · `planning_identity`

That is also the registration order in `build_default_adapters`. `mutuals_register` is registered **before** `planning_identity` so newly ingested societies are already in the graph when the exact-name join runs in the same pass.

---

### `[enricher.ch]` — Companies House OSINT Adapters

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api_key` | string? | `""` | Companies House **REST** API key for the PSC beneficial-ownership and Disqualified Directors adapters. **Set via env** (see below). If absent, empty or whitespace-only (`is_enabled()` returns false), both CH adapters are disabled at startup — the enricher continues without them. |

```toml
[enricher.ch]
# Set via URBAN__ENRICHER__CH__API_KEY environment variable — never commit a real key.
api_key = ""
```

> **The two Companies House keys are TYPED and NOT interchangeable.**
>
> | Env var | Type | Consumed by |
> |---------|------|-------------|
> | `URBAN__INGEST__CH_API_KEY` | **Streaming** key | `serve` only (CH streaming ingestor against `stream.companieshouse.gov.uk`) |
> | `URBAN__ENRICHER__CH__API_KEY` | **REST** key | `ch_psc` and `ch_disqualified` enricher adapters; `urban-integrity-cli backfill ch` / `cessation` / `officers`; and the Python scripts (`ch_backfill.py`, `pa_link_officers.py`, `ch_cessation_backfill.py`, `resolve_planning_applicants_to_ch.py`, `ch_officer_coverage.py`, `officer_merge_audit.py`, `analyst_probe.py`) |
>
> A REST key presented to the streaming endpoint returns `401 Unauthorized`, and the reverse also fails. Issue, scope and rotate the two keys independently, and do not let a shell profile export one under the other's name — a `.bashrc` shadowing incident of exactly that shape once broke `serve` silently.

**Graceful degradation:** If `[enricher.ch].api_key` is absent, empty, or whitespace-only, `ChEnricherConfig::is_enabled()` returns `false`. Both `PscAdapter` and `DisqualifiedAdapter` call `is_enabled()` before opening any connection — each emits a `WARN` log, and the engine's `run_all_enabled` loop continues to the remaining adapters.

---

### `[enricher.land_registry]` — HM Land Registry CCOD Adapter

Controls the CCOD (Corporate Ownership of Land: Domestic) bulk-CSV adapter. The adapter downloads the CCOD ZIP from the HMLR "Use Land and Property Data" service, parses it, and writes `Property` nodes and `identity_links` edges into the graph. An absent or empty `api_key` disables the adapter gracefully. **Coverage: England and Wales only** — the adapter is refused for a Scottish or Northern Irish analysis.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api_key` | string? | `""` | HMLR "Use Land and Property Data" API key (Bearer token). **Set via `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` env var — never commit.** Absent or whitespace-only disables the CCOD adapter (`is_enabled()` returns false). |
| `cache_dir` | string? | `"data/ccod"` | Local directory for downloaded CCOD ZIPs. Created on first run if absent. |
| `ccod_refresh_interval_days` | u32 | `30` | Days between CCOD re-downloads. Matches the monthly HMLR publication cadence. |

```toml
[enricher.land_registry]
# HMLR Use Land and Property Data API key — set via URBAN__ENRICHER__LAND_REGISTRY__API_KEY
api_key = ""
cache_dir = "data/ccod"
ccod_refresh_interval_days = 30
```

**Getting a HMLR API key:**
1. Register at [use-land-property-data.service.gov.uk](https://use-land-property-data.service.gov.uk/)
2. Request access to the **CCOD** (Corporate Ownership of Land: Domestic) dataset
3. Export: `export URBAN__ENRICHER__LAND_REGISTRY__API_KEY="<your HMLR key>"`

**Graceful degradation:** If `api_key` is absent, empty, or whitespace-only, `LandRegistryConfig::is_enabled()` returns `false` and the engine's `run_all_enabled` loop skips the adapter. No `Property` nodes appear in the graph until the key is set.

**SECURITY:** The HMLR API key is never logged. Internal diagnostics log only `api_key_present = true/false`.

**Cache behaviour:** On first run the adapter downloads the current CCOD ZIP from HMLR into `cache_dir` as `CCOD_FULL_current.zip`; the directory is created if absent. On every later run the decision is made from that file's **mtime** alone: if it is younger than `ccod_refresh_interval_days` the adapter makes **no HTTP request at all** and parses the cached ZIP. HMLR is never asked whether a newer edition exists, so a run inside the window re-ingests last month's data and reports success. To force a refresh, delete `CCOD_FULL_current.zip` (or lower the interval) rather than waiting for the adapter to notice.

> **Operational caution — CCOD bulk ingest is the largest memory event in this system.** It is what exposed the unbounded-memory storage engine. Launch SurrealDB only via `scripts/surreal-start.sh` (see "Launching SurrealDB") so the ingest runs against RocksDB inside the capped `urban-surreal` unit.

> **Why the jurisdiction gate refuses rather than warns for CCOD.** The adapter filters rows on `company_number` alone with no geography test, so a Scottish analysis would receive real *English* property for its Scottish companies and the land-flip gate would score it as local corroboration.

---

### `[enricher.ocod]` — HM Land Registry OCOD Overseas-Ownership Adapter

Controls the OCOD (Overseas Companies that Own Property) bulk-CSV adapter. It mirrors the CCOD adapter but covers UK property held by overseas-registered companies. Both CCOD and OCOD are licensed under a single HMLR account. **Coverage: England and Wales only.**

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api_key` | string? | `""` | HMLR "Use Land and Property Data" API key for the OCOD dataset. **Set via `URBAN__ENRICHER__OCOD__API_KEY` env var**, or leave blank to inherit the shared `[enricher.land_registry]` key. SECURITY: never logged (only `api_key_present = true/false`). |
| `cache_dir` | string? | `"data/ocod"` | Local directory for downloaded OCOD ZIPs. Created on first run if absent. |
| `ocod_refresh_interval_days` | u32 | `30` | Days between OCOD re-downloads. Matches the monthly HMLR publication cadence. |

```toml
[enricher.ocod]
# HMLR OCOD key — set via URBAN__ENRICHER__OCOD__API_KEY, or leave blank to
# inherit the [enricher.land_registry] key (one HMLR account licenses both).
api_key = ""
cache_dir = "data/ocod"
ocod_refresh_interval_days = 30
```

**Shared-key fallback:** `OcodConfig::is_enabled()` reports only whether this section's *own* `api_key` is non-empty. The registration site (`build_default_adapters`) resolves the *effective* key — its own `api_key` if non-blank, else `[enricher.land_registry].api_key` — before constructing the adapter, so leaving `[enricher.ocod].api_key` blank while `[enricher.land_registry].api_key` is set still runs OCOD. A whitespace-only key is treated as absent.

**Cache behaviour:** Identical to CCOD, against `{cache_dir}/OCOD_FULL_current.zip` and `ocod_refresh_interval_days`. A cached ZIP younger than the interval means the adapter does **not** re-download and does not contact HMLR; delete the file to force a refresh.

**Licence gate:** OCOD ingestion requires a signed HMLR licence. If your HMLR account does not have it, leave the adapter disabled (no key set).

**Country normalisation:** country-of-incorporation strings are canonicalised against `data/country_incorporation_canonical.json`, which is **compiled into the binary** (see "Committed reference data files"). An unmapped country is retained verbatim and logged as `OCOD: unmapped country-of-incorporation — retained verbatim`; extend the JSON file and rebuild to map it.

> **Do not run `purge-orphans` against a database holding OCOD data.** OCOD records are overseas-owned property with no UK ownership edge of the shape the original orphan definition assumed, and the command once misclassified 93,052 of them as orphans. The definition has been corrected (ownership-edge based), but the `--apply` delete path is separately known to be a silent no-op — verify against live data before relying on it either way.

---

### `[enricher.mutuals]` — FCA Mutuals Public Register Adapter

Ingests the FCA's register of **registered societies** — corporate bodies constituted under the Co-operative and Community Benefit Societies Act 2014 that are **not companies**. They carry a letter-suffixed registration number (`22360R`) unrelated to any Companies House number, and a society with charitable rules is an *exempt charity* absent from the Charity Commission register too. Without this adapter the entire class — housing associations, co-operatives, community benefit societies — is invisible to every other adapter, despite being among the largest recipients of public land and public money in Wales.

The whole register is one **unauthenticated CSV on Azure blob storage**: no API key, no rate limit, no scraping, and no sidecar involvement. **Coverage: UK-wide.**

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Whether the adapter runs. `MutualsRegisterAdapter::is_enabled()` returns this value verbatim — there is no credential gate. |
| `url` | string | `https://fcastoragemprprod.blob.core.windows.net/societylist/SocietyList.csv` | Bulk society list. `mutuals.fca.org.uk` itself is an HTML search form; the bulk file is linked from its footer and is the only sane ingest path. |
| `cache_dir` | string? | `"data/mutuals"` | Local cache directory for the downloaded CSV, stored as `SocietyList.csv`. |

```toml
[enricher.mutuals]
enabled = true
# url = "https://fcastoragemprprod.blob.core.windows.net/societylist/SocietyList.csv"
cache_dir = "data/mutuals"
```

This sub-section is absent from the shipped `config.toml`, so the defaults above are in effect — including `enabled = true`.

**Cache behaviour differs from the HMLR adapters: there is no refresh interval.** If `{cache_dir}/SocietyList.csv` exists it is used unconditionally and no HTTP request is made. To pick up a newer register, delete that file. A download is given a 180-second timeout; if the cache directory is not writable, or the cache write fails, the adapter logs a `WARN` and continues from the in-memory copy rather than failing the pass.

**Encoding:** the file is Windows-1252, not UTF-8, and is decoded leniently. Byte `0x92` (a curly apostrophe, common in names such as `Working Men's Club`) is not valid UTF-8 and aborts a strict decoder mid-file — silently losing the tail of the register while appearing to succeed. A mangled apostrophe is survivable; a truncated register is not.

**What it writes:** each society becomes an `Entity` node keyed `GB-MUT:{registration}` with `company_number: None` (a society number is not a CH number and must not be mistakable for one downstream), plus a `Location` node and `LocatedAt` edge derived from the registered address, so societies are visible to address clustering on the same terms as companies. `Society Status = Deregistered` maps to a dissolved entity status, making societies first-class Phoenix counterparties. The adapter creates **no** identity link of its own — the `planning_identity` join, which runs later in the same pass, handles applicant matching by exact normalised name.

---

### `[enricher.london_gazette]` — London Gazette Insolvency Adapter

Public REST API — no API key required. Always enabled. **Coverage: UK-wide** (all editions). Controls the wall-clock budget and lookback window for each enrichment pass. The whole sub-section is optional; if absent, `LondonGazetteConfig::default()` applies (300 s budget, 30-day lookback).

**The shipped `config.toml` does not leave this section commented out.** As of 2026-08-13 it is active and sets `max_pass_secs = 1800`, overriding the code default. Keep the two apart when reading the table below: **Default (code)** is what `default_gazette_max_pass_secs()` / `default_gazette_lookback_days()` in `crates/core/src/config.rs` supply when the key is absent, and **Shipped value** is what this repository's `config.toml` actually sets. A checkout of this repo runs Gazette passes at 1800 s, not 300 s.

| Key | Type | Default (code) | Shipped value | Description |
|-----|------|----------------|---------------|-------------|
| `max_pass_secs` | u64 | `300` | `1800` | Wall-clock budget for a single enrichment pass in seconds. The code default covers roughly one ~75-fetch rate-limit batch. Override via `URBAN__ENRICHER__LONDON_GAZETTE__MAX_PASS_SECS`. |
| `lookback_days` | i64 | `30` | `30` | Maximum days to look back when the cursor is empty or older than this window. Override via `URBAN__ENRICHER__LONDON_GAZETTE__LOOKBACK_DAYS`. |

```toml
# As shipped in config.toml. The code default for max_pass_secs is 300 —
# this block overrides it.
[enricher.london_gazette]
max_pass_secs = 1800
lookback_days = 30
```

**Why the shipped value is 1800 and not the 300 default:** at a 300 s budget a pass cleared roughly one results page — 314–415 s were observed, so the budget expired before the crawl reached a checkpoint boundary — and draining a backlog cost one pass per page. The cursor is still checkpointed at every page / 75-fetch boundary, so the larger budget stays resumable and bounded; it simply does more work per invocation. Lower it back towards the default if you want shorter, more frequent passes.

**Graceful degradation:** The adapter is always enabled (no key gate). If the Gazette API is unreachable, the pass ends at budget expiry and the cursor is not advanced — the next run retries from the same point. There is no deep backfill: `compute_published_from` starts an empty cursor at `today - lookback_days` (30 by default) and clamps an older cursor to that same floor, so the first run covers the same span as every later one. A 2020-01-01 cursor resolves to 30 days ago.

> `scripts/enrich.sh` prints a pre-run warning that hardcodes "(default 300s)". That is the *code* default; the committed `config.toml` sets `1800`, so the figure the script prints is not the budget the run will actually use.

---

### `[enricher.charity_commission]` — Charity Commission Bulk Download Adapter

OGL v3 open data — no API key required. Enabled by default. **Coverage: England and Wales only.** This sub-section is absent from the shipped `config.toml` — the defaults below apply.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Whether the adapter is active. Set to `false` to disable. |
| `cache_dir` | string? | `None` → `data/charity` | Local directory for downloaded Charity Commission ZIPs. The config field defaults to `None`; the registration site substitutes `data/charity` when unset. |
| `refresh_interval_days` | u64 | `30` | Days between re-downloads. Matches the monthly publication cadence. |

```toml
[enricher.charity_commission]
enabled = true
# cache_dir = "data/charity"   # optional; engine default when unset
refresh_interval_days = 30
```

---

### `[enricher.icij]` — ICIJ Offshore Leaks Adapter

ODbL open data — no API key required. Enabled by default. **Coverage: UK-wide.** This sub-section is absent from the shipped `config.toml` — the defaults below apply.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Whether the adapter is active. |
| `cache_dir` | string? | `None` → `data/icij` | Local directory for downloaded ICIJ ZIPs. The config field defaults to `None`; the registration site substitutes `data/icij` when unset. |

```toml
[enricher.icij]
enabled = true
# cache_dir = "data/icij"   # optional; engine default when unset
```

ICIJ rows are held in isolated `icij_*` tables to keep ODbL-licensed data separate from OGL-licensed data. The load is one-shot and idempotent — it is skipped when `icij_offshore_entity` is already populated.

> **Verify writes actually landed, not just that the log says so.** This adapter once reported 814k rows written while its table stayed empty: the row shape did not satisfy the schema and every write error was discarded. If you change the adapter, `count()` the target table after a run.

---

### `[enricher.opensanctions]` — OpenSanctions Yente Sidecar Adapter

Controls the OpenSanctions `yente` sidecar integration. Requires explicit opt-in and licence confirmation. **Disabled by default.** Coverage: UK-wide.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Whether the adapter is active. Requires `non_commercial_confirmed = true`. |
| `endpoint` | string | `http://localhost:8766` | Yente sidecar endpoint. |
| `non_commercial_confirmed` | bool | `false` | **Must be set to `true`** to confirm CC-BY-NC non-commercial use compliance. Absent → `false` → adapter disabled with a compliance error. |
| `batch_size` | usize | `50` | Batch size for yente `/match` requests. |

```toml
[enricher.opensanctions]
enabled = true
endpoint = "http://localhost:8766"
non_commercial_confirmed = true   # Required — confirms CC-BY-NC compliance
batch_size = 50
```

> **Licence gate:** `non_commercial_confirmed` must be explicitly `true`. The adapter will not activate if this field is `false` or absent — it logs a compliance error instead. OpenSanctions data is CC-BY-NC; do not use it commercially without a commercial licence.

The externally hosted `yente` matcher is the one remaining approximate-matching component in the system, retained as a sanctions/PEP **compliance** class rather than as an entity-resolution mechanism. It does not reintroduce in-app fuzzy matching. <!-- VERIFY: yente sidecar deployment — the localhost:8766 default is a loopback convention; any hosted deployment is an infrastructure detail not expressed in the repository -->

---

### `[enricher.electoral_commission]` — Electoral Commission Donations Register Adapter

OGL v3 open data — no API key required. Enabled by default. **Coverage: UK-wide.** This sub-section is absent from the shipped `config.toml` — the defaults below apply.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Whether the adapter is active. |
| `cache_dir` | string? | `None` → `data/electoral` | Local directory for cached Electoral Commission CSV downloads. The config field defaults to `None`; the registration site substitutes `data/electoral` when unset. |

```toml
[enricher.electoral_commission]
enabled = true
# cache_dir = "data/electoral"   # optional; engine default when unset
```

**No donor fuzzy-match config:** the individual-donor fuzzy/phonetic name matcher was removed on 2026-06-13. This adapter links donations to existing entities only on exact identifiers — there are no similarity-threshold or auto-link/pending-review score knobs to configure. It still reads `URBAN__ENRICHER__CH__API_KEY` when corroborating company donors against Companies House.

**Date format — no knob, but an operational gotcha.** The EC donations feed publishes `AcceptedDate` as `DD/MM/YYYY` (e.g. `20/12/2010`), not ISO. The adapter and the downstream donation-correlation detector both parse it through one shared helper, `ec_date::parse_ec_date` (`crates/enricher/src/ec_date.rs`), which accepts `DD/MM/YYYY` and `YYYY-MM-DD`. The adapter normalises to ISO before writing `evidence_json.accepted_date`, falls back to the raw feed string when neither format parses, and logs a `WARN` naming the `ECRef`. Rows ingested before this change hold `DD/MM/YYYY` in `evidence_json`, which is why both formats are still accepted — nothing needs reconfiguring and no re-ingest is required.

---

### `[enricher.council_registers]` — Council Registers of Interest Adapter

Scrapes councillor registers of interest via the Scrapling sidecar. **Disabled by default** — requires `lpa_codes` to be configured. Coverage: UK-wide.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Whether the adapter is active. Requires at least one entry in `lpa_codes`. |
| `lpa_codes` | string[] | `[]` | Target LPA codes to scrape for register-of-interest declarations (e.g. `["X06999001", "X06999002"]` — fictional; use your authorities' ONS codes). |

```toml
[enricher.council_registers]
enabled = true
lpa_codes = ["X06999001", "X06999002"]   # fictional — your LPAs' ONS codes
```

**Dependency:** Requires the Scrapling sidecar (`localhost:8765`) to be running. If the sidecar is down, errors are logged as `WARN` and the enricher continues to the next adapter.

---

### `[enricher.opencorporates]` — OpenCorporates Cross-Jurisdiction Adapter

Cross-jurisdiction company lookup via the OpenCorporates API. **Disabled by default** — requires explicit opt-in and ODbL licence confirmation. Coverage: UK-wide.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `false` | Whether the adapter is active. Requires `oc_open_data_confirmed = true`. |
| `api_key` | string? | `""` | OpenCorporates API key. **Set via `URBAN__ENRICHER__OPENCORPORATES__API_KEY` env var.** Optional — the free tier works without a key for some endpoints. |
| `oc_open_data_confirmed` | bool | `false` | **Must be set to `true`** to confirm ODbL licence terms. Absent → `false` → adapter returns an error with licence guidance. |
| `monthly_limit` | u32 | `200` | Monthly request cap. Default 200 (free tier). Set to `500` on the Essentials paid tier. |

```toml
[enricher.opencorporates]
enabled = true
# api_key set via URBAN__ENRICHER__OPENCORPORATES__API_KEY
oc_open_data_confirmed = true   # Required — confirms ODbL licence compliance
monthly_limit = 200             # 200 = free tier; 500 = Essentials paid tier
```

> **Licence gate:** `oc_open_data_confirmed` must be explicitly `true`. The adapter enforces a monthly quota counter in SurrealDB — it will not exceed `monthly_limit` requests regardless of how many enrichment cycles run.

---

### `[enricher.ppd]` — HM Land Registry Price Paid Data Adapter

Controls the Price Paid Data (PPD) property-transaction staging adapter. PPD is OGL v3 open data — **no API key required**. **Coverage: England and Wales only.** The 5.1 GB national `pp-complete.txt` is never targeted; only yearly files (`pp-{year}.txt`) for the last `years_back` years are downloaded, and a postcode-district allowlist filters rows at load time. Raw rows are staged into `ppd_transactions` only; the land-flip correlation is a separate pass (`--source land-flip`).

This is the one `[enricher.*]` sub-section carrying `#[serde(deny_unknown_fields)]` — a typo in a key name here is a hard parse error rather than a silently ignored setting.

| Key | Type | Struct default | Shipped value | Description |
|-----|------|----------------|---------------|-------------|
| `enabled` | bool | `false` | `false` (enable it in `config.local.toml` once you have set your districts) | Whether the adapter is active. `PpdConfig::is_enabled()` returns this flag verbatim — PPD is open data, so no API-key check is performed. |
| `years_back` | u32 | `5` | `5` | Number of past years of yearly PPD files to download. Each yearly file is ~800 MB; only in-scope rows are retained after the district filter. |
| `postcode_districts` | string[]? | `None` (derive from graph) | `["XX1", "XX2", "XX3"]` — placeholders, `XX` is not a UK postcode area | Allowlist of postcode districts. When present it **overrides** the dynamic derivation from `Location`-node postcodes. |
| `cache_dir` | string? | `None` → `data/ppd` | unset | Local directory for cached PPD text files. The registration site substitutes `data/ppd` when unset. |

```toml
# In config.local.toml, for a real analysis:
[enricher.ppd]
enabled = true
years_back = 5
# cache_dir = "data/ppd"
# The outward codes (part before the space) covering your LPA. Nothing validates
# the format — a typo simply matches no rows.
postcode_districts = ["XX1", "XX2", "XX3"]
```

> **`postcode_districts` must stay EXPLICIT. Do not delete it to "let the graph derive it".**
>
> Measured on 2026-08-01 against a single-council analysis, deriving the allowlist from `Location` nodes yields **3,202 districts** — CF14, E14, NW10 and 3,158 others — because the CCOD and OCOD adapters write UK-wide *registered office* addresses into `location`. That derived allowlist passes essentially every PPD row in the country, which is the multi-million-row load that previously wedged SurrealDB.
>
> It also buys nothing: the land-flip detector correlates PPD transactions against planning applications, and every planning application in a single-council analysis is in that council's area — that analysis needed 21 districts. The dynamic-derivation fallback remains in the code for a future analysis whose `location` table is not polluted by national ownership datasets — it is not safe to rely on today.

**Environment variable overrides:** `URBAN__ENRICHER__PPD__ENABLED` and `URBAN__ENRICHER__PPD__YEARS_BACK` follow the standard `URBAN__<SECTION>__<KEY>` pattern.

> **Never full-reingest PPD to test idempotency.** A redundant full `enrich --source ppd` re-pass thrashes SurrealDB's background tasks (`Transaction conflict: Resource busy`) — one such run spun 19 hours at ~2% CPU, bloated the store to ~9.9 GB and wedged the query engine. Idempotency is structural (UPSERT on `transaction_id`); confirm it with an unchanged `count()` or a single-year re-run. Recovery: kill only the `enrich` client; if queries still hang while `/health` is up, `systemctl --user stop urban-surreal` then `scripts/surreal-start.sh` (the launcher will not replace a healthy-but-wedged instance, so stop it first).

> **Bind dates as typed values, never as strings.** SurrealDB 3.x will not coerce a string into a `datetime` field, and a `CONTENT` write awaited without `.take(0)?` / `.check()` swallows the coercion failure. A PPD run once reported "2795151 written" against a staging table holding zero rows. This is a code-level rule, not a configuration knob, but it is the failure mode a PPD misconfiguration most resembles.

---

## Committed Reference Data Files

`data/*` is gitignored, with three files force-added back in. None is a `config.toml` section and none is overridable by a `URBAN__*` variable.

| File | Read how | Purpose |
|------|----------|---------|
| `data/country_incorporation_canonical.json` | **`include_str!`** at compile time (`crates/enricher/src/country_canonical.rs`) | Canonicalises country-of-incorporation strings for the OCOD adapter. Editing it requires a rebuild. Malformed JSON panics at first use rather than surfacing as a config error. |
| `data/formation_agent_postcodes.txt` | **`include_str!`** at compile time (`crates/detector/src/motif/cluster.rs`) | Postcodes belonging to company-formation agents, excluded from address-cluster scoring so a mass-registration address does not read as a corruption motif. Overridable per-test via `AddressClusterConfig.formation_agent_postcodes`; not overridable at runtime. |
| `data/party_authority_control.json` | **Runtime read** from a hardcoded relative path (`PartyAuthorityMap::load()` in `crates/enricher/src/donation_correlation.rs`) | Maps party → controlled LPA codes for the donation-correlation detector. See below. |

### `data/party_authority_control.json` — Donation-Correlation Detector Input

The path is resolved against the process's working directory, so the enricher must be run from the repository root. Failure to load is non-fatal: `run_donation_correlation` logs a `WARN` and skips the detector entirely.

The file maps a party name to the list of LPA codes that party controls. Its `_meta.name_variants` object additionally maps Electoral Commission *registered legal* names onto the colloquial keys used for the lists (`"Conservative and Unionist Party"` → `"Conservative Party"`). That alias table is consulted on lookup — exact key first, then alias (matched case-insensitively), then a normalised comparison. Adding a party the EC publishes under a different name therefore means adding **both** the top-level key and its `_meta.name_variants` entry; a party name that resolves to nothing is skipped at the party gate and every donation for it drops out of the detector.

> **This file is synthetic placeholder data. Do not read its contents as real council-control information.**
>
> It parses cleanly, but the 44 authority codes it contains are perfectly sequential English LPA codes — `E06000001`–`E06000009`, `E07000001`–`E07000015`, `E08000001`–`E08000006`, `E09000001`–`E09000014` — handed to parties in unbroken runs. That is generated filler, not a survey result, notwithstanding a `_meta` header naming Open Council Data UK and a 2025 post-election date. Welsh coverage is **zero**: no `W06…` authority appears at all.
>
> The consequence is not a clean no-op. The codes are used only to label the alert the detector writes (`lpa_code` in the `MotifDetected` payload); they do **not** filter which contracts are considered. On a Welsh analysis any alert emitted therefore attributes a Welsh contract to an unrelated English authority. Replace this file with sourced council-control data covering the authorities in your analysis before treating `donation_contract_correlation` alerts as evidence. <!-- VERIFY: real council control per LPA is externally published data (e.g. Open Council Data UK) and is not derivable from this repository -->

The detector's remaining parameters are compile-time constants rather than configuration: a **730-day** window in both directions between donation and contract award (revolving-door detection), a `LIMIT 1000` cap on contracts queried per donor, a fixed risk score of `0.8` (HIGH), and a gate that considers only `identity_links` rows with `source = 'electoral_commission'` and `status IN ['auto_linked','confirmed']`.

---

## Local LLM (Ollama) — `SummarizeAlert`

The CLI alert/report summarisation path (`crates/cli/src/ollama.rs`) calls an Ollama OpenAI-compatible endpoint. The URL and model are hardcoded constants, **not** `config.toml` or `URBAN__*` keys:

```rust
pub const OLLAMA_URL: &str = "http://localhost:11434/v1";   // request model: "gemma4"
```

Ollama is **optional**. `summarize_via_ollama` returns `Option<String>` and degrades gracefully — if Ollama is unreachable or returns an error, the function returns `None` and summarisation is simply skipped, with no hard failure path. There is no configuration knob for the Ollama host, port, or model; to point at a different host you must edit the constant. <!-- VERIFY: Ollama deployment host/port — the loopback default localhost:11434 is hardcoded; any non-local Ollama deployment is an infrastructure detail not expressed in the repository -->

---

## Environment Variable Reference

| Variable | Maps to | Notes |
|----------|---------|-------|
| `URBAN__ANALYSIS` | `analysis` (top-level) | Selected analysis id (registry `db_name` — a **database** name); `main` or unset → `[surreal].database`. Set automatically by the global `--analysis` CLI flag, and honoured by the Python scripts. One exception: `export-graphml --source events` (the default source) reads `[surreal].database` directly and ignores this selector — see "`export-graphml` — command-specific flags". |
| `URBAN__SURREAL__WS_URL` | `surreal.ws_url` | Rewritten `ws://`→`http://` by the CLI before connecting |
| `URBAN__SURREAL__USERNAME` | `surreal.username` | Prefer env over config file; also used by the 50-minute enrich re-auth (engine + WAL drain sessions) and `reauth_db()` |
| `URBAN__SURREAL__PASSWORD` | `surreal.password` | Prefer env over config file; also used by the 50-minute enrich re-auth (engine + WAL drain sessions) and `reauth_db()` |
| `URBAN__SURREAL__NAMESPACE` | `surreal.namespace` | The namespace — not the analysis |
| `URBAN__SURREAL__DATABASE` | `surreal.database` | The default database when no analysis is selected |
| `URBAN__WORKBENCH__BIND_ADDR` | `workbench.bind_addr` | Workbench HTTP bind address (default `127.0.0.1:8090`). Overridden in turn by `workbench --bind`. A taken port is a startup error, never a silent re-pick. |
| `URBAN__JURISDICTION__NATION` | `jurisdiction.nation` | Explicit nation override; refused at startup if it contradicts the configured wards |
| `URBAN__JURISDICTION__ALLOW_MIXED_NATIONS` | `jurisdiction.allow_mixed_nations` | `true` permits wards from more than one nation and leaves the analysis nation unknown |
| `URBAN__INGEST__CH_API_KEY` | `ingest.ch_api_key` | CH **streaming** key — required by the CH streaming ingestor under `serve`; omit to disable. Not used by any `backfill` subcommand (those need the REST key). 401s if a REST key is used here. |
| `URBAN__INGEST__FIRECRAWL_API_KEY` | `ingest.firecrawl_api_key` | Optional; enables the Rust-side Firecrawl fallback in `ingest-planning` (Idox portals only) |
| `URBAN__INGEST__SCRAPLING_TIMEOUT_SECS` | `ingest.scrapling_timeout_secs` | Per-request sidecar `/scrape` timeout (default 300) |
| `URBAN__INGEST__PLANNING_POLL_INTERVAL_SECS` | `ingest.planning_poll_interval_secs` | Ward re-scrape interval (default 3600) |
| `URBAN__GRAPH__WAL_CHANNEL_CAPACITY` | `graph.wal_channel_capacity` | Tune for memory/backpressure tradeoff |
| `URBAN__DETECTOR__PHOENIX_WINDOW_DAYS` | `detector.phoenix_window_days` | Phoenix lookback window (default 180) |
| `URBAN__DETECTOR__IMD_CSV_PATH` | `detector.imd_csv_path` | Deprivation-index CSV path; unset disables IMD weighting. An index belonging to another nation is refused at startup. |
| `URBAN__ENRICHER__ENRICH_INTERVAL_SECS` | `enricher.enrich_interval_secs` | Override enrichment sleep interval; default 86400 |
| `URBAN__ENRICHER__CH__API_KEY` | `enricher.ch.api_key` | CH **REST** key for the PSC + Disqualified Directors adapters, `backfill ch` / `cessation` / `officers`, and the Python scripts; distinct from the streaming key; omit to disable both adapters and hard-fail those backfills. 401s if a streaming key is used here. |
| `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` | `enricher.land_registry.api_key` | HMLR "Use Land and Property Data" Bearer token for the CCOD bulk-CSV adapter; omit to disable |
| `URBAN__ENRICHER__OCOD__API_KEY` | `enricher.ocod.api_key` | HMLR Bearer token for the OCOD overseas-ownership adapter; omit to inherit the `land_registry` key |
| `URBAN__ENRICHER__MUTUALS__ENABLED` | `enricher.mutuals.enabled` | FCA Mutuals Public Register adapter toggle; defaults to `true` (no credential required) |
| `URBAN__ENRICHER__MUTUALS__URL` | `enricher.mutuals.url` | Override the bulk society-list CSV URL |
| `URBAN__ENRICHER__MUTUALS__CACHE_DIR` | `enricher.mutuals.cache_dir` | Cache directory for `SocietyList.csv` (default `data/mutuals`) |
| `URBAN__ENRICHER__LONDON_GAZETTE__MAX_PASS_SECS` | `enricher.london_gazette.max_pass_secs` | Wall-clock budget per Gazette pass (code default 300; the shipped `config.toml` sets 1800) |
| `URBAN__ENRICHER__LONDON_GAZETTE__LOOKBACK_DAYS` | `enricher.london_gazette.lookback_days` | Gazette lookback window when the cursor is empty (default 30) |
| `URBAN__ENRICHER__OPENCORPORATES__API_KEY` | `enricher.opencorporates.api_key` | OpenCorporates API key; optional for free-tier endpoints |
| `URBAN__ENRICHER__PPD__ENABLED` | `enricher.ppd.enabled` | Enable the PPD adapter (open data, no key) |
| `URBAN__ENRICHER__PPD__YEARS_BACK` | `enricher.ppd.years_back` | Number of past years of yearly PPD files to download (default 5) |
| `RUST_LOG` | — | Tracing level filter (e.g. `urban_integrity=debug,warn`); defaults to `info`. All tracing output goes to **stderr** so structured stdout stays parseable. |
| `RUST_LOG_FORMAT` | — | Set to `json` for structured JSON log output (production / `serve`); any other value or unset gives the human-readable format (`crates/core/src/telemetry.rs`). |

Any other `URBAN__<SECTION>__<KEY>` follows the same double-underscore nesting and overrides the matching `config.toml` value.

**Not in the `URBAN__*` namespace:** the SurrealDB launcher and guard variables (`URBAN_SURREAL_*`, `SURREAL_BIN`, `URBAN_INTEGRITY_DIR`, `SURREAL_GUARD_BYPASS` — single underscore, read by shell scripts), the sidecar variables (`SIDECAR_PORT`, `SIDECAR_HOST`, `SIDECAR_ALLOWED_HOSTS`, `FIRECRAWL_API_KEY`, `URBAN_SIDECAR_PYTHON`), `URBAN_CLI` (read by `scripts/export_graphml.sh` and `scripts/export_parquet.sh` to point at a prebuilt CLI binary instead of running `cargo build`), and `RUST_LOG` / `RUST_LOG_FORMAT`. The three committed reference data files sit outside the config system altogether — hardcoded paths, two of them resolved at compile time, with no key and no override.

### Test-suite gates

Read only by test code; not part of the runtime configuration.

| Variable | Effect |
|----------|--------|
| `URBAN_DB_TESTS=1` | Runs the SurrealDB integration tests. The gate is a skip-**by-return**, so gated and ungated runs report the same passing count — the only observable difference is duration. Requires a SurrealDB instance reachable at `ws://localhost:8000`. CI sets it, so the suite cannot silently rot. |
| `URBAN_TEST_DB_URL` | Overrides the endpoint the test harness connects to (default `http://localhost:8000`), used by `urban-integrity-test-support`. |
| `URBAN_LIVE_TESTS=1` | Runs tests that hit Companies House / Contracts Finder over the network. Not in CI (needs a CH REST key and depends on third-party availability). |
| `URBAN_PLANNING_TESTS=1` | Runs tests that call the real Scrapling sidecar on `:8765`. |

---

## Python Forensic Scripts Configuration

The scripts in `scripts/` connect directly to SurrealDB over HTTP and do not use the Rust engine's config loading. They share a resolver module, `scripts/surreal_target.py`, which mirrors the Rust CLI's selection precedence.

### Shared target resolution (`surreal_target.py`)

Precedence, highest first:

1. `--analysis <db_name>` (explicit flag)
2. `URBAN__ANALYSIS`
3. `URBAN__SURREAL__DATABASE`
4. `[surreal].database` in `config.local.toml` if it exists, else `config.toml` — the same file choice as `AppConfig::load()`

**There is deliberately no default.** If none of the four resolves, the script exits with an error rather than guessing, and `preflight()` verifies the database actually exists before any work begins. A silent fallback is what made the old hardcoded `"main"` dangerous: SurrealDB creates a database on first use, so a script pointed at a non-existent name writes into a fresh empty one, finds nothing, and reports success.

Connection settings resolve the same way as the Rust loader, falling back to `[surreal]` in the config file in effect (`config.local.toml`, else `config.toml`) and then to hardcoded values:

| Variable | Fallback (config.toml, then hardcoded) |
|----------|----------------------------------------|
| `URBAN__SURREAL__WS_URL` | `ws://localhost:8000` (rewritten to HTTP) |
| `URBAN__SURREAL__NAMESPACE` | `urban_integrity` |
| `URBAN__SURREAL__USERNAME` | `root` |
| `URBAN__SURREAL__PASSWORD` | `root` |

The same namespace/database distinction applies here as in the Rust path: `URBAN__ANALYSIS` selects the **database**; `URBAN__SURREAL__NAMESPACE` selects the namespace.

Scripts importing `surreal_target` directly: `ch_backfill.py`, `ch_officer_coverage.py`, `fix_edge_directions.py`, `normalize_officer_keys.py`, `pa_ingest.py`, `pa_link_officers.py`, `phoenix_funnel.py`, `projection_audit.py`, `repair_psc_officer_links.py`.

Scripts reusing `analyst_probe.SurrealClient`, and therefore its resolution behaviour: `cf_backfill_dates.py`, `ch_cessation_backfill.py`, `corroborate_offsite_applicant_matches.py`, `land_flip_funnel.py`, `land_flip_pairable.py`, `rekey_officers_to_director_slug.py`, `resolve_planning_applicants_to_ch.py`, `split_false_merged_officers.py`.

### Companies House REST scripts

`ch_backfill.py`, `ch_cessation_backfill.py`, `ch_officer_coverage.py`, `officer_merge_audit.py`, `pa_link_officers.py`, `resolve_planning_applicants_to_ch.py` and `analyst_probe.py` resolve entities against the CH REST API.

| Variable | Default | Description |
|----------|---------|-------------|
| `URBAN__ENRICHER__CH__API_KEY` | — | Companies House **REST** API key. **Required** by the scripts above (in `analyst_probe.py` it only widens coverage). `ch_backfill.py` and `pa_link_officers.py` exit with `ERROR: set URBAN__ENRICHER__CH__API_KEY (the REST key) or pass --api-key`; others raise `URBAN__ENRICHER__CH__API_KEY not set (REST key, not streaming)`. The streaming key (`URBAN__INGEST__CH_API_KEY`) is **not** valid here — it 401s against the REST endpoint. |

```bash
export URBAN__ENRICHER__CH__API_KEY="<your CH REST key>"
python3 scripts/ch_backfill.py --analysis <db_name> [--dry-run]
python3 scripts/pa_link_officers.py --analysis <db_name> [--dry-run]
python3 scripts/resolve_planning_applicants_to_ch.py [--apply] [--limit N]
python3 scripts/ch_cessation_backfill.py [--fetch] [--apply]
```

`resolve_planning_applicants_to_ch.py` derives the analysis's operating region from the wards it was actually given rather than hardcoding it; a candidate whose registered office falls outside those areas is withheld for review rather than rejected outright.

### `analyst_probe.py`

Runs an adaptive battery of SurrealDB queries to detect Phoenix loops, address clusters, and nepotism bridges, then writes a report to `evidence-report.md` at the project root (relative to the script location: `scripts/../evidence-report.md`).

```bash
python3 scripts/analyst_probe.py                  # probe + write evidence-report.md
python3 scripts/analyst_probe.py --no-report      # probe only, no file written
python3 scripts/analyst_probe.py --json out.json  # dump raw findings as JSON
```

`--json` is independent of `--no-report` (mirroring the Rust `probe` command's decoupling of `--json` from `--output`) and **requires** a path argument: a bare trailing `--json`, or `--json --no-report`, exits with `error: --json requires a file path argument` rather than raising or writing a file named after the next flag.

It reads its SurrealDB connection from `[surreal]` in `config.local.toml` (else `config.toml`) with the same env overrides, and **does** honour the analysis selector: `URBAN__ANALYSIS`, then `URBAN__SURREAL__DATABASE`, then `[surreal].database`. There is no `"main"` fallback — if none resolves, the probe exits with `error: no analysis database selected`. Its `SurrealClient` is reused by the scripts listed above, so those inherit the same behaviour.

No CH API key is required to run the probe, but coverage is limited when Officer and PoliticalActor nodes are absent (those are populated by the backfill scripts). The Land Registry CCOD probe reports a `DATA_GAP` verdict until `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` is set and the CCOD adapter has run.

### `build_deprivation_ward_index.py`

Builds the file `[detector].imd_csv_path` points at. Takes no environment configuration — everything is command-line. See "The deprivation index file" under `[detector]`.

---

## Minimal Production Config

Illustrative only. The repository ships no deployment manifest — there is no Dockerfile, Compose file or Kubernetes chart in the tree — so the hostnames (`surreal`, `scrapling-sidecar`) and the `/var/lib/urban-integrity/...` paths below are conventions, not values any file in this repository sets. Substitute whatever your deployment actually uses.
<!-- VERIFY: production hostnames, data paths and the `production` database name in this example — infrastructure details with no source of truth in the repository -->

```toml
[surreal]
ws_url = "ws://surreal:8000"
namespace = "urban_integrity"   # namespace: one per install
database = "production"         # database: the default analysis target

[graph]
checkpoint_events_n = 5000
checkpoint_interval_secs = 300
checkpoint_path = "/var/lib/urban-integrity/checkpoints"
wal_channel_capacity = 50000
wal_batch_size = 500
wal_batch_window_ms = 50
shard_count = 128
snapshot_every_n_events = 5000
snapshot_interval_secs = 60

[ingest]
cf_poll_interval_secs = 300
scrapling_url = "http://scrapling-sidecar"
scrapling_port = 8765
scrapling_timeout_secs = 300

[detector]
phoenix_window_days = 180
phoenix_min_value = 25000.0
# ONE NATION PER FILE, and it must match the nation of the configured wards or
# startup is refused. Build it with scripts/build_deprivation_ward_index.py.
imd_csv_path = "/var/lib/urban-integrity/data/wimd_2019_wards.csv"

# [jurisdiction] is normally omitted — the nation derives from the ward codes.

# Optional; shown with its default. The workbench binds loopback unless told otherwise.
[workbench]
bind_addr = "127.0.0.1:8090"

[enricher]
enrich_interval_secs = 86400

[enricher.ch]
# api_key set via URBAN__ENRICHER__CH__API_KEY (REST key)

[enricher.land_registry]
# api_key set via URBAN__ENRICHER__LAND_REGISTRY__API_KEY
cache_dir = "/var/lib/urban-integrity/data/ccod"
ccod_refresh_interval_days = 30

[enricher.ocod]
# api_key set via URBAN__ENRICHER__OCOD__API_KEY (or inherits land_registry key)
cache_dir = "/var/lib/urban-integrity/data/ocod"
ocod_refresh_interval_days = 30

[enricher.mutuals]
# Public unauthenticated CSV — no credential. Leave enabled: registered societies
# are invisible to every other adapter.
enabled = true
cache_dir = "/var/lib/urban-integrity/data/mutuals"

[enricher.london_gazette]
# 1800 is what this repo's config.toml ships; the code default is 300. At 300 s
# a pass clears about one results page, so a backlog drains a page per pass.
max_pass_secs = 1800
lookback_days = 30

[enricher.charity_commission]
enabled = true

[enricher.icij]
enabled = true

[enricher.electoral_commission]
enabled = true

# PPD: open data, no key. Keep postcode_districts EXPLICIT — deriving it from
# Location nodes passes essentially the whole national dataset.
[enricher.ppd]
enabled = true
years_back = 5
postcode_districts = ["XX1", "XX2"]   # placeholders — your LPA's outward codes

# OpenSanctions: disabled until the yente sidecar is deployed and licence confirmed
[enricher.opensanctions]
enabled = false
non_commercial_confirmed = false

# OpenCorporates: disabled until ODbL licence confirmed
[enricher.opencorporates]
enabled = false
oc_open_data_confirmed = false
```

Set secrets via environment (values shown are placeholders — never commit real keys):
```bash
export URBAN__SURREAL__USERNAME="<db user>"
export URBAN__SURREAL__PASSWORD="<db password>"
export URBAN__INGEST__CH_API_KEY="<CH STREAMING key>"          # serve only
export URBAN__ENRICHER__CH__API_KEY="<CH REST key>"            # ch_psc, ch_disqualified, backfill, python scripts
export URBAN__ENRICHER__LAND_REGISTRY__API_KEY="<HMLR key>"    # CCOD adapter
export URBAN__ENRICHER__OCOD__API_KEY="<HMLR key>"             # optional — inherits land_registry key
export RUST_LOG_FORMAT=json                                    # structured logs for serve
```

---

## Firecrawl API Key

`firecrawl_api_key` in `[ingest]` configures the paid Firecrawl fallback used when the Scrapling sidecar returns no results. It is unrelated to any Companies House key and is optional.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `firecrawl_api_key` | string? | `""` | Firecrawl API key — **set via env**. If absent or empty, the fallback is skipped. |

```toml
[ingest]
# Set via URBAN__INGEST__FIRECRAWL_API_KEY environment variable
firecrawl_api_key = ""
```

**Where the fallback actually lives:** in the Rust `ingest-planning` command (`crates/cli/src/commands/ingest_planning.rs`), not in the sidecar. Per ward it tries the sidecar first, and falls through to `scrape_idox_via_firecrawl()` only when **all three** hold: the sidecar returned zero items, `firecrawl_api_key` is non-empty, and `portal_type == "idox"`. If both paths return nothing the ward is recorded as `⚠ no data (sidecar + firecrawl both failed or portal empty)` and the run continues.

> The fallback is inert for any analysis whose portals are all `portal_type = "arcus"` (or another non-Idox type), regardless of whether a key is set. It only becomes reachable once an Idox portal is configured.

The Python sidecar has its own, separate Firecrawl integration gated on a plain `FIRECRAWL_API_KEY` environment variable in the sidecar's own process (see "Scrapling sidecar process configuration"). Setting `URBAN__INGEST__FIRECRAWL_API_KEY` does not configure the sidecar.

**Getting a Firecrawl API key:**
1. Register at [firecrawl.dev](https://firecrawl.dev) <!-- VERIFY: Firecrawl registration URL -->
2. Copy the API key from your dashboard
3. Export: `export URBAN__INGEST__FIRECRAWL_API_KEY="<your firecrawl key>"`

| Variable | Maps to | Notes |
|----------|---------|-------|
| `URBAN__INGEST__FIRECRAWL_API_KEY` | `ingest.firecrawl_api_key` | Rust-side fallback in `ingest-planning`; Idox portals only |
| `FIRECRAWL_API_KEY` | — (sidecar process env) | Sidecar-side integration; not read from `config.toml` |

---

## `.env` File Support

The config loader calls `dotenv::dotenv()` at startup (`crates/core/src/config.rs`, `AppConfig::load()`), so a `.env` file in the project root is loaded automatically before environment variables are processed. **Secrets are never committed** — `.env` is gitignored, and it is where credentials such as `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` live locally. A `.env.example` file is provided at the repository root as a template.

Copy `.env.example` to `.env` and fill in real values:

```bash
# .env — copy of .env.example with real values (placeholders shown)
URBAN__SURREAL__USERNAME=<db user>
URBAN__SURREAL__PASSWORD=<db password>
URBAN__INGEST__CH_API_KEY=<CH streaming key>
URBAN__ENRICHER__CH__API_KEY=<CH REST key>
URBAN__ENRICHER__LAND_REGISTRY__API_KEY=<HMLR key>
```

> **Note — always use the double underscore after `URBAN`.** The loader is `Environment::with_prefix("URBAN").separator("__")`, so only `URBAN__…` names are read; a single-underscore name such as `URBAN_SURREAL__USERNAME` is **silently ignored**, with no warning and no parse error. (Older copies of `.env.example` shipped exactly that mistake; the current file uses `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` and lists the optional API keys commented out.)

Variables defined in `.env` follow the same `URBAN__*` naming convention as shell exports. The `.env` file is gitignored — never commit it with real credentials.
