<!-- generated-by: gsd-doc-writer -->
# Analyst Workbench

> **⚠️ Work in progress — this software makes mistakes.** Its alerts, scores and reports are
> leads for qualified review, never evidence of wrongdoing. Do not accuse any person or
> organisation on the basis of its output unless a qualified fraud investigator is involved and
> you have taken legal advice first. See [DISCLAIMER.md](../DISCLAIMER.md).

The Analyst Workbench is a browser UI for **one analysis database**. It exists so that a person can
work through the data and find things themselves. It does not hand down verdicts of its own. It
**replays data that is already stored**: at boot it rebuilds the graph from `graph_events` into a
read-only engine, and every page reads that engine or the analysis database. The workbench never
ingests, enriches or runs the detectors, so it cannot change what the graph says. It writes only
three things:

- the cached probe report (`workbench_report:latest`)
- the saved-investigation tables (`saved_investigation`, `saved_investigation_pin`)
- alert withdrawals, which go through the existing withdrawal path

This page is the operator and contributor reference: how to build it, run it and QA it, what each
page does, and how the code is laid out. For the boot sequence, server components and CI guards,
see the **Analyst Workbench** section of [ARCHITECTURE.md](ARCHITECTURE.md#analyst-workbench).
That section is not repeated here.

## Crates

| Crate | Path | Role |
|-------|------|------|
| `urban-integrity-workbench` | `crates/workbench` | Native Axum server. Handles replay-only engine boot (`boot.rs`), JSON handlers (`analysis`, `alerts/`, `entity/`, `graph/`, `investigation`, `home`, `status`, `population`) and graph algorithms over the CSR snapshot (`graph/algorithms/`). It also produces the Arrow IPC population export and serves the `rust-embed` static assets (`assets.rs`) |
| `urban-integrity-workbench-dto` | `crates/workbench-dto` | Wire types shared by server and browser. Depends on `serde` only, so it compiles natively and for `wasm32-unknown-unknown` |
| `urban-integrity-workbench-web` | `crates/workbench-web` | Leptos 0.8 client-side-rendered (CSR) frontend (`leptos_router`, `gloo-net`), bundled by Trunk. It may depend **only** on `workbench-dto` plus the Leptos / wasm-bindgen stack. `scripts/check-wasm-boundary.sh` enforces this |

The `workbench` subcommand of `urban-integrity-cli` (`crates/cli/src/commands/workbench.rs`)
launches the workbench. It resolves the analysis and the canonical population, then hands off to
`urban_integrity_workbench::boot`.

## Building

`cargo build --workspace` builds the **server** without a frontend. If `crates/workbench/dist/` is
missing, `assets.rs` serves a `200 OK` stub page headed *"Workbench assets not built"* (its `<h1>`; the `<title>` is *"UrbanIntegrity Workbench"*). To get the
real UI you also have to build the frontend bundle.

### Prerequisites

For the whole ordered setup from a fresh clone (OS packages, SurrealDB, build, first run on the
planted fixture), follow [GETTING-STARTED.md Part A](GETTING-STARTED.md#part-a--fresh-clone-to-a-running-workbench).
The workbench-specific parts are:

- **The `wasm32-unknown-unknown` target.** `rust-toolchain.toml` (channel `1.96.0`) lists it under
  `targets`, so rustup normally installs it on first use. `scripts/build-workbench.sh` checks
  anyway and stops with `rustup target add wasm32-unknown-unknown` if it is missing.
- **The Trunk wasm bundler, version 0.21.14:**

  ```bash
  cargo install --locked trunk --version 0.21.14
  ```

- **Watch for the `pg-trunk` name clash.** `pg-trunk`, a PostgreSQL extension manager, also
  installs a binary called `trunk`. Its `trunk build` means something unrelated. The build script
  runs `trunk --version` and refuses to continue if the output starts with `pg-trunk`, or does not
  start with `trunk`. If a machine needs both tools, install the bundler into its own root and put
  that root first on `PATH` for the build. For example:

  ```bash
  cargo install --locked trunk --version 0.21.14 --root ~/.local/trunk-bundler
  PATH="$HOME/.local/trunk-bundler/bin:$PATH" scripts/build-workbench.sh
  ```

  A bare `cargo install trunk` replaces whichever `trunk` is already in `~/.cargo/bin`.

### Build the frontend

```bash
scripts/build-workbench.sh
```

This script is the only place Trunk runs. There is no `build.rs` hook. It runs `trunk build --release` inside
`crates/workbench-web`. `crates/workbench-web/Trunk.toml` sets `dist = "../workbench/dist"`, so
the bundle is written straight into `crates/workbench/dist/`, which is the folder `rust-embed`
reads. `crates/workbench/dist/` is gitignored. `index.html` pulls in `style/tokens.css` through
a `data-trunk` link.

### Re-embed the bundle into the server binary

`crates/workbench/Cargo.toml` enables `rust-embed`'s `debug-embed` feature. Assets are therefore
compiled into the binary in **every** profile and are never read from disk per request. As a
result, **after a frontend rebuild the server binary must be rebuilt too**. Otherwise it keeps
serving the previous bundle. A change under `dist/` does not always trigger a recompile of
`crates/workbench`, so force one:

```bash
scripts/build-workbench.sh
touch crates/workbench/src/lib.rs
cargo build -p urban-integrity-cli
```

Before any visual check, compare the `.wasm` file the running server serves with the one in
`crates/workbench/dist/`. A stale embed looks exactly like a frontend bug.

> Both `scripts/build-workbench.sh` and any `cargo` command are heavy builds. Run them one at a
> time, not in parallel.

## Running

```bash
urban-integrity-cli workbench [--analysis <db_name>] [--bind <addr>] [--open]
# from a checkout:
cargo run -p urban-integrity-cli -- workbench --analysis <db_name>
```

| Option | Source | Behaviour |
|--------|--------|-----------|
| `--analysis <id>` | global CLI flag (bridged to `URBAN__ANALYSIS`) | Picks the analysis database to replay. Resolution is the same as `serve`. With no flag, the `config.toml` `[surreal] database` is used |
| `--bind <addr>` | `WorkbenchArgs.bind` | Overrides `[workbench] bind_addr` in `config.toml`, whose shipped value is `127.0.0.1:8090`. The environment override is `URBAN__WORKBENCH__BIND_ADDR`. The CLI flag takes precedence over both |
| `--open` | `WorkbenchArgs.open` | Opens the URL in the default browser (`xdg-open`, or `open` on macOS) once the port is bound. Off by default. A failed launch only logs a warning |

Startup behaviour worth knowing:

- **The port is bound first**, before any SurrealDB round-trip. A port that is already taken fails
  at once with a named cause. There is no automatic fallback port.
- The only thing printed is `Workbench: http://<bind_addr>`, and only once the socket exists. A
  browser tab opened early waits in the kernel backlog while `graph_events` replays. On a large
  corpus the replay can take minutes.
- The workbench is bound to one database for the life of the process. No request parameter can
  select another. In the header's analysis picker, choosing a different analysis shows the restart
  command you need to run. It does not switch analyses in place.
- SurrealDB must be reachable at the `config.toml` `[surreal]` endpoint. See
  [CONFIGURATION.md](CONFIGURATION.md).

**Troubleshooting a busy port.** Workbench processes left over from earlier sessions can hold
`127.0.0.1:8090`. Before a QA session, run `ss -ltnp | grep 8090` and stop any leftover
`urban-integrity-cli workbench` process. If a `cargo run` parent is killed, its child can keep
running and keep the port.

## QA on the planted toy analysis

Manual QA runs against a small **planted fixture** analysis whose correct answers are known in
advance, never against a live corpus.

```bash
scripts/toy-analysis.sh
# then, using the slug it prints:
cargo run -p urban-integrity-cli -- workbench --analysis <slug>
```

The script is idempotent. It:

1. checks that SurrealDB answers on `http://127.0.0.1:8000/health`
2. reads the fixture's ward code from `crates/test-support/src/planted.rs` (`WARD_CODE`)
3. looks up an analysis labelled `Planted fixture` with `analysis list`, or creates one with
   `analysis create --label "Planted fixture" --wards-file <tmp> --fixture`
4. reseeds it with `--analysis <slug> fixture seed --planted --reseed`
5. projects it with `project`
6. runs the probe battery with `probe --output data/toy-evidence-report.md`

The script runs four `cargo run` invocations when the analysis already exists and six when it has to create it, so it counts as a cargo build. **Reseed before each
QA session.** A toy database left over from an older fixture looks like a fixture that is
incomplete.

The CLI `probe` step writes only the Markdown report. The workbench's `/analysis` page reads the
report cached in `workbench_report:latest`, and only a run started from the page writes that
cache. Use the page's **Run** button to fill it in.

## Page-by-page tour

Every page renders under the app shell header. The header sits outside the route table and holds
the app mark, the **Investigations** and **Alerts** links, the analysis picker (fed by
`/api/analyses`) and the theme toggle. Every page except Home shows a breadcrumb. Routes are
defined in `crates/workbench-web/src/app.rs`. An unknown path renders "Not found.".

| Route | Component (file) | What it shows |
|-------|------------------|---------------|
| `/` | `HomePage` (`home_page.rs`) | Launcher grid of four cards: **Analysis**, **Graph**, **Ingestors** and **Help**. Each card is filled from `GET /api/home` |
| `/analysis` | `AnalysisPage` (`analysis_page.rs`) | The probe findings report. See below |
| `/finding/:probe` | `FindingDetailPage` (`finding_detail_page.rs`) | One finding, deep-linkable. See below |
| `/alerts` | `AlertsPage` (`alerts/mod.rs`) | The alert / motif triage inbox. See below |
| `/alert/:id` | `AlertDetailPage` (`alert_detail_page.rs`) | One alert in full. See below |
| `/graph` | `GraphPage` (`graph_page.rs`) | Interactive population canvas and algorithm tool rail. See below |
| `/entity` | `EntitySearchPage` (`profile/`) | Entity search, with a disambiguation list when a name matches several entities (`/api/entity/disambiguate`) |
| `/entity/:id` | `EntityProfilePage` (`profile/`) | Entity profile. See below |
| `/investigations`, `/investigations/:id` | `InvestigationsPage`, `InvestigationPage` (`investigation/page.rs`) | Saved investigations. See below |
| `/help`, `/help/glossary`, `/help/algorithms`, `/help/algorithms/:slug` | `help/` | The in-app guide (reading the report, the verdict vocabulary, graph algorithms, CLI pivots), a glossary of every abbreviation shown in the UI, and one help page per algorithm |

**Analysis (`/analysis`).** Opens with a BLUF ("bottom line up front") box built from the report's
headline counts (`BlufView`) and a **Run** button that calls `POST /api/analysis/run`. Results are
split into six fixed groups that always render: *Confirmed Findings*, *Suggestive Patterns*,
*Data-Integrity Gaps*, *Not Applicable*, *Verified Negative Findings* and *Skipped*. ("Confirmed
Findings" holds the probes whose test matched the data — `HIT` verdicts. It does not mean any
wrongdoing has been confirmed; see [DISCLAIMER.md](../DISCLAIMER.md).) Each group is
an accordion of probes, and only one probe is open at a time. A URL hash (`/analysis#<probe-name>`)
opens one probe directly. The Markdown version of the report is available from
`GET /api/analysis/report.md`. If a second run is started while one is in progress, the server
returns `409 Conflict`.

**Finding detail (`/finding/:probe`).** Reached from a finding card's "View finding" link. It takes
the finding's hypothesis, verdict and evidence from the report already fetched, without querying
again. For a finding shaped like a graph motif (a star or a typed edge), it also fetches the
neighbourhood of the evidence records (`GET /api/entity/neighbourhood?ids=…&hops=…`) and mounts
the graph tool rail, scoped to that neighbourhood.

**Alerts inbox (`/alerts`).**

- **Default view.** Opens on *every* alert. A filter that is not set is left out of the query, so
  there is no hidden minimum score.
- **Filters.** The rail on the left uses structured pickers only, with no free-text query box:
  - detector (motif) and ward, each single-select, with per-option counts
  - a minimum and maximum `P_c` score, each bounded to 0–1 with −/+ stepper buttons
  - a from/to date range

  A footnote gives the number of alerts that carry no ward code. A ward filter can never match
  those alerts.
- **Sort.** A segmented control switches between *score* (the default) and *recency*
  (`sort=recency`).
- **Paging.** **Show more** appends the next page. Changing a filter or the sort goes back to the
  first page. The server's default page size is 50 and its maximum is 1000.
- **Withdrawn alerts.** Hidden by default, with a count of how many are hidden. A toggle shows
  them (`include_withdrawn=true`).
- **Row actions.** Each row draws its motif. **Withdraw…** requires a written reason, checked in
  the browser before anything is sent. A pin control adds the alert to the open investigation.
- **Noise floor.** A line reporting the minimum-participant floor, as measured by the server.

**Alert detail (`/alert/:id`).** Shows the motif at full size, the complete participants list, and
facts including the `P_c` score band, the IMD (deprivation index) state and which run produced the
alert. The graph tool rail is seeded from the alert's participants. The withdraw and pin actions
are the same components as in the inbox. An unknown id shows a not-found message, not an error.

**Graph (`/graph`).**

- **Population panel.** Population metadata from `GET /api/graph/population/meta`.
- **Canvas.** A Cosmograph canvas loaded once from the Arrow IPC population export
  (`GET /api/graph/population`). It is drawn through the vendored shim `js/cosmograph-shim.js`.
  The browser parses the Arrow data with its own small decoder because the `arrow` crate cannot
  be part of the wasm build.
- **Node side card.** Clicking a node opens a side card (`GET /api/graph/node`).
- **Canvas filters.** Structured pickers only, applied through `POST /api/graph/canvas-filter`.
- **Tool rail.** Holds an algorithm picker, parameter forms, an edge-type checklist, a legend and
  Run / Cancel (`POST /api/graph/algorithm`, `POST /api/graph/algorithm/cancel`). The algorithms
  are `all_simple_paths`, `shortest_paths`, `community` (seeded Louvain), `k_core`, `betweenness`
  and `ward_subgraph`. The browser can also send `highlight_set`.
- **Entity picker.** A typeahead (`GET /api/entities/search`) fills the From, To and Ward fields.
- **Results.** A results list and a status line report each run.

**Entity profile (`/entity/:id`).** A sectioned profile (`GET /api/entity/profile`) with an as-at
date control. Every row carries a provenance chip. References to other entities are links only
when the target can open a profile, which today means `Entity` and `OffshoreEntity`. Other record
kinds, such as contracts, planning applications and locations, render as plain text, never as
dead links. The neighbourhood of the entity can be expanded (`GET /api/entity/neighbourhood`),
starting at 1 hop. This profile component is also shown, in a compact layout, in the canvas
inspection panel.

**Investigations (`/investigations`, `/investigations/:id`).**

- **Storage.** Investigations are stored in the analysis database, not in the browser. A reload or
  a workbench restart loses nothing.
- **What you can save.** You can pin entities or edges from a profile, the canvas, or the alerts
  pages. You can also save a whole subgraph (`POST /api/investigation/subgraph`) and add notes to
  the investigation and to each pin.
- **The open investigation.** Opening an investigation also makes it the one that new pins go to.
  Which investigation is open is kept in memory for the session only.

## HTTP API

`build_router` in `crates/workbench/src/boot.rs` merges the workbench endpoints onto
`api::core_router`. `core_router` keeps `/health`, `/metrics`, `/graphql` and `/graphiql`. Any
path that matches no route goes to `assets::static_handler`, which serves the embedded
single-page app.

| Method | Path | Query / body | Purpose |
|--------|------|--------------|---------|
| GET | `/api/workbench-status` | — | Engine and population status for the bound analysis |
| GET | `/api/analyses` | — | `_catalog` analysis list for the picker, excluding retired and retiring analyses |
| GET | `/api/home` | — | Data for the Home cards |
| GET | `/api/analysis/report` | — | Cached probe report (`ReportView`) |
| POST | `/api/analysis/run` | — | Runs the probe battery in-process. Returns `409` if a run is already in progress |
| GET | `/api/analysis/report.md` | — | The same report as Markdown |
| GET | `/api/graph/population` | — | Population export as Arrow IPC: a node stream (`id`, `kind`, `name`) and a link stream (`source`, `target`, `edge_type`) |
| GET | `/api/graph/population/meta` | — | Population metadata |
| GET | `/api/graph/node` | `id` | Summary of one node |
| GET | `/api/entities/search` | `q` | Entity typeahead |
| POST | `/api/graph/algorithm` | `AlgorithmRequest` (`algorithm`, `session_id`, `edge_types_included`, `seed`, `from_id`, `to_id`, `ward_id`, `ids`, …) | Runs a graph algorithm |
| POST | `/api/graph/algorithm/cancel` | session | Cancels a running algorithm |
| POST | `/api/graph/canvas-filter` | `CanvasFilterRequest` | Applies canvas filters |
| GET | `/api/entity/profile` | `id`, `as_at`, `section`, `cursor`, `page_size` | Entity profile, with paging per section |
| GET | `/api/entity/disambiguate` | `q` | Candidates for an ambiguous name |
| GET | `/api/entity/neighbourhood` | `id` or `ids`, `hops`, `node_cap` | Neighbourhood around one entity or several |
| GET / POST | `/api/investigations` | — / create body | List or create investigations |
| GET / PATCH / DELETE | `/api/investigation` | `id` | Read, update (name, notes) or delete one investigation |
| POST / PATCH | `/api/investigation/pin` | pin body | Pin an entity or edge, or edit a pin's note |
| POST | `/api/investigation/unpin` | pin body | Remove a pin |
| POST | `/api/investigation/subgraph` | subgraph body | Save a subgraph into an investigation |
| GET | `/api/alerts` | `ward`, `motif`, `min_score`, `max_score`, `from`, `to`, `sort` (`recency`; otherwise score), `start`, `limit` (default 50, max 1000), `include_withdrawn` | One page of alerts, with facets, a count of hidden withdrawn alerts and the noise-floor line |
| GET | `/api/alert/{id}` | — | One alert. Returns `404` for an unknown id |
| POST | `/api/alert/{id}/withdraw` | reason | Withdraws an alert, with the reason required |

The request and response types are in `crates/workbench-dto/src/`:

- `report_view.rs`, `verdict_view.rs`: the probe report and its verdicts
- `alerts.rs`: alert pages and facets
- `entity_profile.rs`, `neighbourhood.rs`, `disambiguation.rs`: entity views
- `algorithm.rs`, `canvas_filter.rs`: graph requests and results
- `investigation.rs`: saved investigations
- `provenance.rs`, `temporal.rs`: provenance and date ranges
- `paging.rs`, `caps.rs`: paging and size limits

Two conventions apply to these types. Request DTOs are strict: a missing field is rejected.
Fields of **persisted** DTOs use `#[serde(default)]`, so that adding a field does not make stored
reports unreadable.

## Design tokens and accessibility

All styling is in `crates/workbench-web/style/tokens.css`, which is plain CSS with no Tailwind and
no CDN stylesheet. Components refer to its custom properties through inline `var(--…)` styles.

- **Themes.** `:root` holds the dark theme, which is the default. The `.light` class overrides it.
  The header toggle cycles System → Light → Dark and saves the choice in `localStorage` under
  `workbench-theme` (`theme.rs`).
- **Colour tokens:**
  - surfaces and text: `--background`, `--card`, `--card-hover`, `--border`, `--foreground`,
    `--muted-foreground`
  - roles: `--primary`, `--accent`, `--destructive`, `--warning`, `--success`
  - tiers: `--tier-confirmed`, `--tier-auto`
  - node categories: `--node-entity`, `--node-officer`, `--node-location`, `--node-contract`,
    `--node-ward`, `--node-property`, `--node-osint`
  - edge categories: `--edge-control`, `--edge-award`, `--edge-risk`, `--edge-place`,
    `--edge-money`, `--edge-property`
- **Spacing:** `--space-xs` (4px), `--space-sm` (8px), `--space-md` (16px), `--space-lg` (24px),
  `--space-xl` (32px), `--space-2xl` (48px) and `--space-3xl` (64px).
- **Typography:**
  - four size roles: `.text-label` (12px/600), `.text-body` (14px/400), `.text-heading`
    (16px/600) and `.text-display` (24px/600)
  - `.font-mono` for counts, timestamps and ids
- **`.control-button`.** The shared style for buttons:
  - a hit target of at least 44×44px
  - a border in `--muted-foreground`. This meets the 3:1 non-text contrast floor, which
    `--border` does not.
  - a visible `:focus-visible` outline
  - `[aria-pressed="true"]` inverts foreground and background, rather than using `--primary`, to
    keep 12px labels above 4.5:1 contrast

  Wrap a row of these buttons in `.segmented` to join them into a segmented control, as the alerts
  sort control does. `.pc-bound-input` hides the native number spinner on the `P_c` fields, which
  have their own stepper buttons.
- **Conventions:**
  - interactive controls get an `aria-label` (the theme toggle names the current mode) or an
    `aria-pressed` state
  - abbreviations shown in the UI go through the `Abbr` component and must have a glossary entry,
    which `scripts/check-abbreviations.sh` checks
  - UI copy contains no analysis-specific literals, which `scripts/check-no-analysis-literals.sh`
    checks
  - a count that is not known is shown as unavailable, never as a made-up `0`

## Tests

No headless browser is set up, so frontend logic is written as plain functions and tested on the
host with ordinary `#[test]`s inside each module. The integration tests under
`crates/workbench-web/tests/` read the crate's **source code** to check that components are
actually mounted. A component can be fully unit-tested and still never be rendered, and these
tests exist to catch that.

| Test (`crates/workbench-web/tests/`) | Guards |
|--------------------------------------|--------|
| `routes_render.rs` | Every route in the route table renders without panicking |
| `abbr_reachable.rs` | `Abbr` is used somewhere in the rendered component tree |
| `pivot_reachable.rs` | Every surface that shows entity references renders them as links, for exactly the kinds that can open a profile |
| `expansion_reachable.rs` | Neighbourhood expansion and the compact profile layout are actually used |
| `canvas_filter_reachable.rs` | The canvas filter controls are mounted, and visibility is defined in one place |
| `investigation_reachable.rs` | The pin control is mounted, the investigation list can be reached from the header, and a pin is defined in one place |
| `alert_filters_rail_stable.rs` | The `/alerts` filter rail is mounted once and is not rebuilt when facets update (rebuilding it reset decimal input) |
| `alert_controls_affordance.rs` | Contrast calculations and CSS rules for `.control-button` and the sort control, read from `tokens.css` |
| `alert_detail_preview.rs` | `/alert/:id` draws the alert's own motif, not the participant-chip fallback |
| `cvd_node_palette.rs` | A colour-vision-deficiency simulation over the seven `--node-*` tokens in both themes |

Server-side tests are in `crates/workbench/tests/`:

- **Router-level tests** start `build_router` on an ephemeral port:
  - `entity_api.rs`
  - `investigation_api.rs`, which checks that an investigation survives a restart
  - `canvas_filter_api.rs`
- **Golden tests against the planted fixture:**
  - `planted_golden.rs`
  - `planted_profile.rs`
  - `planted_examples_match_manifest.rs`, which checks that the help-page worked examples are facts
    about the fixture
- **DB-gated proofs** that need `URBAN_DB_TESTS=1` and a live SurrealDB:
  - `report_persistence.rs`
  - `report_view_equals_finding_list.rs`
  - `population_equals_export.rs`
  - `finding_star_golden.rs`
- **`regenerate_population_arrow.rs`** is `#[ignore]`d. Run it on purpose to rebuild the real
  Arrow fixture at `crates/workbench-web/src/testdata/population-real-arrow.bin`:

  ```bash
  cargo test -p urban-integrity-workbench --test regenerate_population_arrow -- --ignored
  ```

In CI, the `wasm-build` job in `.github/workflows/ci.yml` builds `workbench-dto` and
`workbench-web` for `wasm32-unknown-unknown` and runs the boundary and source guard scripts.
[ARCHITECTURE.md](ARCHITECTURE.md#analyst-workbench) lists them. For how to run the suites and
use the DB gate, see [TESTING.md](TESTING.md).
