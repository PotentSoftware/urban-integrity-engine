<!-- generated-by: gsd-doc-writer -->
# Getting Started

<!-- GSD-MANAGED-DOC: getting_started -->

> **⚠️ Work in progress — this software makes mistakes.** Its alerts, scores and reports are
> leads for qualified review, never evidence of wrongdoing. Do not accuse any person or
> organisation on the basis of its output unless a qualified fraud investigator is involved and
> you have taken legal advice first. See [DISCLAIMER.md](../DISCLAIMER.md).

This guide takes you from a fresh clone on a clean machine to the analyst workbench running in
your browser, **with no API keys and no data**, then shows where to go for real data. It is in two
parts:

- **[Part A — Fresh clone to a running workbench](#part-a--fresh-clone-to-a-running-workbench)**,
  in the order you should do it: prerequisites, toolchain, build, SurrealDB, a first run on the
  synthetic *planted fixture*, tests, then pointers for real data and troubleshooting.
- **[Part B — Reference](#part-b--reference)**: the operational rules and the numbered sections
  (configuration, deprivation index, sidecar, `serve`, reports, enrichment, analyses, workbench)
  that Part A links into.

**A fresh clone has no data.** The SurrealDB store, the deprivation index and every download
cache live under `data/`, which is gitignored. The committed `config.toml` is a fictional example:
its default database (`[surreal] database = "an_example_00000000"`) is just a label for an empty
database, and its planning portals are commented-out examples. For real data you copy it to the
gitignored `config.local.toml` and edit that (step A7). You create your own analyses (Part A, step 5 and step 7). Any row count or
timing quoted in this guide comes from a populated maintainer instance, not from anything you will
see on a fresh clone.

---

## Part A — Fresh clone to a running workbench

### A0. Machine sizing

| Resource | Guidance |
|----------|----------|
| RAM | 16 GB minimum, 32 GB comfortable. `rustc` peaks at about **3 GB per process** on this workspace, and an uncapped build on a many-core machine starts one per core. SurrealDB itself needs little memory for the toy analysis, but a real ingest can push it to its 12 GB cap. |
| Disk | Several tens of GB free. The debug `urban-integrity-cli` binary alone is around 800 MB and `target/` grows well beyond that; a real corpus under `data/` adds tens of GB more. |
| OS | **Linux with a systemd user session** is the supported, tested platform (the SurrealDB and sidecar launchers use `systemd-run --user` to cap memory). Without one (macOS, WSL without systemd, containers) both launchers fall back to a detached background process with no memory cap; see step A4. Windows outside WSL is untested. |

**Cap cargo's parallelism before your first build**, so one build cannot starve the machine. Add
this to `~/.cargo/config.toml` (create it if needed; pick a number that is roughly RAM ÷ 4 GB):

```toml
[build]
jobs = 8
```

That caps a *single* cargo invocation. It does nothing about two at once, so **run one cargo
command at a time** — and count `scripts/toy-analysis.sh` (several `cargo run`s),
`scripts/build-workbench.sh` (a release wasm build) and `cargo install` as cargo commands too.

### A1. Install the OS prerequisites

`scripts/check-prereqs.sh` checks everything in this step and prints the install command for
whatever is missing on your platform. Run it after cloning, and again whenever a build or a
database test behaves oddly — a missing SurrealDB does **not** fail `cargo build` or `cargo test`
(the DB-gated tests skip silently), so nothing else will tell you it is absent.

| Prerequisite | Needed for | Source of truth |
|--------------|------------|-----------------|
| C toolchain (`gcc`/`cc`, `make`) | Native build scripts (`aws-lc-sys`, `zstd-sys`, `ring`) | `Cargo.lock` |
| `pkg-config` + OpenSSL headers (Linux only) | `openssl-sys`, pulled in through `reqwest`'s `native-tls` — not vendored | `Cargo.lock` |
| `protoc` (Protocol Buffers compiler) | `crates/api/build.rs` compiles `proto/integrity.proto` with `tonic-build`; the build fails without it | `crates/api/build.rs`, `.github/workflows/ci.yml` |
| `curl`, `git` | Installers, the launchers' health checks, cloning | `scripts/*.sh` |
| `rustup` | Installs the pinned toolchain from `rust-toolchain.toml` | `rust-toolchain.toml` |
| SurrealDB **v3.1.0-beta.2** server binary | The database; the Rust SDK is pinned to `=3.1.0-beta.2` | `Cargo.toml`, `.github/workflows/ci.yml` |
| Trunk **0.21.14** (the wasm bundler) | Building the workbench's browser frontend | `.github/workflows/ci.yml`, `scripts/build-workbench.sh` |
| systemd user session (Linux) — recommended | Lets `scripts/surreal-start.sh` and `scripts/sidecar-start.sh` cap memory; both fall back to a background process without one | the scripts themselves |
| *Optional:* Python 3.11+ with `venv` | The Scrapling sidecar, the deprivation-index builder, the Python forensic scripts | `scrapling-sidecar/requirements.txt`, `scripts/*.py` |
| *Optional:* Node.js 20 + npm | **Only** the Cosmograph shim drift guard (`scripts/check-shim-drift.sh`). The workbench build itself does not need Node — the bundle it loads is committed | `.github/workflows/ci.yml` |
| *Optional:* LibreOffice | **Only** `build_deprivation_ward_index.py --nation northern-ireland` (converts NISRA's `.xls`) | `scripts/build_deprivation_ward_index.py` |
| *Optional:* Ollama | **Only** the LLM alert summaries; everything degrades gracefully without it | [CONFIGURATION.md](CONFIGURATION.md) |

**Debian / Ubuntu:**

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev protobuf-compiler curl git \
    python3 python3-venv
```

**Fedora:**

```bash
sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config openssl-devel protobuf-compiler curl git python3
```

**Arch Linux** (and derivatives such as Omarchy, Manjaro, EndeavourOS):

```bash
sudo pacman -S --needed base-devel pkgconf openssl protobuf curl git python
```

**macOS:** install the Xcode command-line tools and `protoc`. OpenSSL is not needed — `native-tls`
uses the system Security framework there.

```bash
xcode-select --install
brew install protobuf
```

**Rust (rustup):**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**SurrealDB v3.1.0-beta.2.** Install that exact version — not v1/v2, and not a newer v3 — because
the schema relies on its beta semantics. Run the official installer **as your normal user, without
`sudo`**:

```bash
curl -sSf https://install.surrealdb.com | sh -s -- --version v3.1.0-beta.2
~/.surrealdb/surreal version     # → 3.1.0-beta.2 for linux on x86_64 (or your platform)
```

The installer writes to `/usr/local/bin` when that is writable and falls back to `~/.surrealdb`
otherwise. `scripts/surreal-start.sh` looks for `~/.surrealdb/surreal` by default; if your binary
landed elsewhere, export `SURREAL_BIN=/path/to/surreal` (or symlink it into `~/.surrealdb/`, which
`scripts/report.sh` also expects).

**Trunk 0.21.14 — the wasm bundler, not `pg-trunk`.** An unrelated PostgreSQL tool, `pg-trunk`,
also installs a binary called `trunk`. Install the bundler by its exact version (this is itself a
cargo build — run it on its own):

```bash
cargo install --locked trunk --version 0.21.14
trunk --version                  # must print "trunk 0.21.14" — NOT "pg-trunk …"
```

If `trunk --version` prints `pg-trunk`, install the bundler into its own root and put it first on
`PATH` whenever you build the frontend:

```bash
cargo install --locked trunk --version 0.21.14 --root ~/.local/trunk-bundler
export PATH="$HOME/.local/trunk-bundler/bin:$PATH"
```

### A2. Clone and set up the toolchain

```bash
git clone https://github.com/PotentSoftware/urban-integrity.git
cd urban-integrity
rustup show        # installs the pinned 1.96.0 + clippy, rustfmt and the wasm32-unknown-unknown target
```

`rust-toolchain.toml` pins the channel to `1.96.0` and lists the `clippy`/`rustfmt` components and
the `wasm32-unknown-unknown` target; `rustup show` (or any first `cargo` command) inside the repo
installs all of it. You do not pick a toolchain by hand. CI does exactly this (`rustup show`).

### A3. Build

Build the browser frontend **first**, then the workspace, so the one native build embeds the
frontend assets:

```bash
scripts/build-workbench.sh       # Trunk → crates/workbench/dist/ (checks trunk and the wasm target first)
cargo build --workspace          # all crates, incl. the urban-integrity-cli binary
```

- The first native build downloads and compiles a large dependency tree; expect tens of minutes
  on a capped laptop. Later builds are incremental. The first Trunk build also downloads a
  matching `wasm-bindgen` CLI, so it needs network access.
- The CLI binary is **`urban-integrity-cli`** (`target/debug/urban-integrity-cli`). This guide runs
  it as `cargo run -p urban-integrity-cli -- <command>`. Some reference sections write the bare
  binary name; to have it on your `PATH`, run `cargo install --locked --path crates/cli` *after*
  `scripts/build-workbench.sh`.
- `cargo build --workspace` works without Trunk; the `workbench` command then serves a
  "Workbench assets not built" page instead of the UI. The frontend assets are baked into the
  binary at compile time, so **after any later `scripts/build-workbench.sh`**, force a re-embed:
  `touch crates/workbench/src/lib.rs && cargo build -p urban-integrity-cli`.

### A4. Start SurrealDB

From the **repository root** — never from a git worktree (see
[Rule 1](#1-start-surrealdb-only-via-scriptssurreal-startsh--never-surreal-start)):

```bash
scripts/surreal-start.sh
curl -sf http://127.0.0.1:8000/health && echo up     # HTTP 200 once the server is ready
scripts/surreal-start.sh --stop                      # stop it again, whichever launcher started it
```

The launcher serves a RocksDB store at `data/surreal-rocks.db`, signed in as `root`/`root` (the
credentials `config.toml` uses), always launched from the repository root so the store lands in
this checkout's `data/`. It is idempotent: re-running it against a healthy instance does nothing.
Logs go to `data/surreal.log`. It picks one of two launchers (`URBAN_SURREAL_LAUNCHER=auto`, the
default):

- **systemd** — when a systemd user session answers (Linux; the supported path). SurrealDB runs as
  a transient `urban-surreal` user service with a hard memory cap (`MemoryMax` 12G, tune with
  `URBAN_SURREAL_MEM_MAX`), so a runaway ingest kills only the database, never your editor or
  desktop. `systemctl --user stop urban-surreal` also stops it.
- **background** — when there is no systemd user session (macOS, WSL without systemd,
  containers). SurrealDB runs as a detached process with its PID in `data/surreal.pid`. It prints
  a warning because **nothing caps its memory**: fine for the toy analysis and the tests, but
  before a real bulk ingest (the HMLR CCOD dataset especially) prefer a machine with systemd. On
  **WSL2** you can turn systemd on (add `[boot]` / `systemd=true` to `/etc/wsl.conf`, then
  `wsl --shutdown`). The `systemctl --user …` commands elsewhere in this guide do not apply; use
  `scripts/surreal-start.sh --stop`.

Force either with `URBAN_SURREAL_LAUNCHER=systemd` or `URBAN_SURREAL_LAUNCHER=background`. (CI uses
a third arrangement: the `surrealdb/setup-surreal` action with an in-memory store.)

**Initialise the schema** (optional — every CLI command applies it on connect):

```bash
cargo run -p urban-integrity-cli -- status
```

On a fresh clone this reports zero graph events and `Projection: never run` for the default
database. That is correct: nothing has been ingested.

### A5. First run: the planted toy analysis (no API keys, no data)

The **planted fixture** is a small synthetic graph (`crates/test-support/src/planted.rs`, placeholder
ward `X05999001`, no real people or companies) built so that the probe battery and the graph
algorithms have something to find. `scripts/toy-analysis.sh` puts it in an analysis of its own:

```bash
scripts/toy-analysis.sh
```

It checks SurrealDB answers on `:8000`, creates an analysis labelled `Planted fixture` with
`analysis create --fixture` (only once), seeds it with `fixture seed --planted --reseed`, runs
`project`, runs the probe battery into `data/toy-evidence-report.md`, and finally prints the
command to start the workbench with the analysis's database name filled in, for example:

```bash
cargo run -p urban-integrity-cli -- workbench --analysis an_planted_fixture_xxxxxxxx --open
```

(Use the exact name the script printed, or look it up with
`cargo run -p urban-integrity-cli -- analysis list`.) The workbench prints its URL —
`http://127.0.0.1:8090` by default — as soon as it is listening; `--open` also opens it in your
browser. Then:

- **Graph** shows the fixture's ward-reachable population on an interactive canvas.
- **Analysis** says no probe has run yet — the CLI `probe` writes Markdown only. Press **Run** to
  run the battery in the workbench; the result is stored and reloaded on the next start.
- **Help** explains the report vocabulary and each graph algorithm.

Stop the workbench with Ctrl-C. Re-run `scripts/toy-analysis.sh` after pulling changes to the
fixture or the probes. More detail: [Section 21](#21-analyst-workbench-browser-ui) and
[WORKBENCH.md](WORKBENCH.md).

### A6. Run the tests

```bash
cargo test --workspace                      # unit + integration; no database needed
URBAN_DB_TESTS=1 cargo test --workspace     # also runs the SurrealDB-gated tests
scripts/check.sh                            # fmt --check, clippy -D warnings, tests (add --db for the gated run)
```

- The gated tests need SurrealDB reachable at `ws://localhost:8000` as `root`/`root` (step A4).
  They create and drop their own `test…` namespaces, so they never touch your analyses.
- **The DB gate is a skip-by-return.** Without `URBAN_DB_TESTS=1`, gated tests return early and
  are *counted as passed*, so both commands report the same pass count. A green ungated run says
  nothing about the database tests.
- **On a fresh clone the gated run is fast**, because the CLI integration tests run against the
  default database from the config file in effect — the example `config.toml`'s, which is empty for
  you — the same situation as CI. Once you have a `config.local.toml`, they use *its* database. The
  "a fast gated run is suspicious" warnings elsewhere in these docs apply to a machine whose store
  holds a real corpus.
- CI additionally runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  a `wasm32-unknown-unknown` build of the browser crates, the workbench guard scripts under
  `scripts/check-*.sh`, and `trunk build --release`. [TESTING.md](TESTING.md) has the full inventory.

### A7. Optional: real data

None of this is needed for the toy analysis. Before starting, read
[How long will it take?](#how-long-will-it-take) below — a first real ingest runs for well over
half a day. Do it in roughly this order:

1. **Create your own analysis.** Put the ONS GSS ward codes you want to study in a file, one per
   line, and run `cargo run -p urban-integrity-cli -- analysis create --label "<your label>" --wards-file wards.txt`.
   It prints the analysis id (`an_<label-slug>_<8 hex>`). See [Section 20](#20-provision-and-manage-analyses).
2. **Make your own config.** Leave the committed `config.toml` alone — it is a fictional example —
   and copy it:

   ```bash
   cp config.toml config.local.toml     # gitignored; never committed
   ```

   When `config.local.toml` exists it is read **instead of** `config.toml` (the two are never
   merged, so keep your copy complete); `URBAN__*` environment variables still override it. In your
   copy:

   - `[surreal] database` — the id from step 1 (or pass `--analysis <id>` on every command).
   - `[[ingest.planning_portals]]` — uncomment the example entries and replace the fictional
     `X06…`/`X05…` codes and `planning.example.invalid` URLs with your wards' ONS codes and your
     council's planning-register URLs, one entry per ward. The ward codes also set the analysis
     nation, which gates the deprivation index and some registries.
     [CONFIGURATION.md](CONFIGURATION.md#ingestplanning_portals--planning-portal-ward-configuration)
     explains where to find the codes and which `portal_type` to use.
   - `[enricher.ppd]` — your authority's postcode districts (outward codes), then `enabled = true`.
   - `[detector] imd_csv_path` — the deprivation index for your nation (step 4).

   See [CONFIGURATION.md](CONFIGURATION.md) for every key and the exact file-precedence rules.
3. **Credentials** — put them in a gitignored `.env` at the repo root (see `.env.example`) or
   export them ([Section 3](#3-configure)). Never put keys in either config file:

   | Variable | Required for | Notes |
   |----------|--------------|-------|
   | `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` | every command | default `root`/`root`, matching the launcher |
   | `URBAN__INGEST__CH_API_KEY` | `serve` (Companies House **streaming** API) | optional; streaming is disabled without it |
   | `URBAN__ENRICHER__CH__API_KEY` | `backfill ch/cessation/officers`, `enrich --source ch_psc/ch_disqualified`, the Python CH scripts (Companies House **REST** API) | a different key from the streaming one — they are not interchangeable |
   | `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` | the HMLR CCOD/OCOD adapters | free registration; adapters skip without it |
   | `URBAN__INGEST__FIRECRAWL_API_KEY` | optional Firecrawl fallback for planning portals | |

   None is required to build, to run the tests, or for the toy analysis.
4. **Deprivation index** for your analysis's nation — `python3 scripts/build_deprivation_ward_index.py --nation <nation> --lad <codes>`
   (standard library only, but it downloads the published index and pages the ONS Postcode
   Directory, so it needs network access). See [Section 4](#4-build-the-deprivation-index).
5. **Scrapling sidecar** for planning-portal scraping — Python venv + `scripts/sidecar-start.sh`.
   See [Section 5](#5-optional-start-the-scrapling-sidecar).
6. **Ingest, project, probe**: `serve` ([Section 6](#6-start-the-engine)), then the enrichment and
   backfill commands ([Sections 11–12](#11-run-the-forensic-investigation-pipeline),
   [18](#18-osint-enrichment-and-identity-link-review)), `project`
   ([Section 13](#13-project-graph-to-native-surrealdb-tables)), and the workbench on your analysis.

#### How long will it take?

These figures come from the maintainer's own laptop working on a single analysis. Treat them as a
rough guide to the order of magnitude, not as a promise: your times depend on how many wards and
companies your analysis covers, external API rate limits, your network, and your disk.

**Reference machine**

| | |
|---|---|
| Laptop | TUXEDO InfinityBook Pro Gen8 |
| OS | TUXEDO OS (Ubuntu-based Linux) |
| CPU | Intel Core i7-13700H — 14 cores / 20 threads |
| RAM | 31 GB |
| Disk | Samsung 980 NVMe SSD |
| Network | fast home broadband (speed not measured) |

**The analysis measured:** one local authority, about 16 wards. At the time of these measurements
it held about 986,000 graph events, which projected to about 358,000 nodes and 507,000 edges.

| Step | Time on the reference machine | What drives it |
|------|-------------------------------|----------------|
| **First full ingest, end to end** | **well over 12 hours** (the maintainer's recollection; not timed as one run) | Mostly the Companies House officer backfill, below |
| Companies House officer backfill (`backfill officers`) | a full pass can take **up to ~18 hours** (one pass covered 14,135 companies) | Companies House allows **600 requests per 5 minutes**, so the commands default to `--rate-limit 2` (2 requests/second). Time grows with the number of companies in your area, not with your hardware |
| `backfill ch` (planning applicants → company numbers) | several hours — roughly 20,000 lookups at 2 requests/second | Same rate limit |
| Later incremental re-ingest | **about 25 minutes** for one measured cycle | Only new or changed records are fetched |
| HM Land Registry CCOD (UK companies' property) | **about 6 minutes** to ingest once downloaded | Local processing of the bulk file |
| Planning re-list of one ward | **up to ~160 s** (157.6 s for a 4,986-application ward) | One sidecar request per ward; `scrapling_timeout_secs` must clear your largest ward |
| `project` (rebuild the queryable graph from the event log) | **about 78–87 minutes** | Not incremental: it rebuilds everything each run. See below |

> **On `project`'s run time:** an earlier run on 2026-08-15 took **2 h 20 m** for about 499,000
> edges ([DEVELOPMENT.md](DEVELOPMENT.md) quotes it), while later runs on a quieter machine took
> 78–87 minutes for a slightly larger graph. Other load on the machine matters at least as much as
> graph size. Plan for 1½–2½ hours, and don't run anything else heavy at the same time —
> `project` also slows concurrent reads.

**Resources**

- **Memory.** For a real analysis use **32 GB of RAM**. The 16 GB minimum in [A0](#a0-machine-sizing)
  is enough for the toy analysis only. SurrealDB's resident memory levels off at about 6 GB under
  heavy load, and `scripts/surreal-start.sh` caps it at 12 GB. Builds need about 3 GB per `rustc`
  process on top of that — keep the `jobs` cap from A0, and don't build while a long ingest or
  `project` is running.
- **Disk.** Use an NVMe SSD. The SurrealDB store for the analysis above was about **2 GB** on disk
  (RocksDB). Download caches under `data/` take far more: Price Paid Data alone is about 800 MB per
  yearly file. Leave tens of GB free.
- **Network.** Downloads are a few large bulk files plus a long series of rate-limited API calls,
  so a steady connection matters more than a fast one.

**Running long steps safely**

- Run anything that takes hours inside `tmux` or `screen`, so it survives a closed terminal or a
  dropped SSH session.
- The `backfill` subcommands are idempotent, so you can re-run them without duplicating data, and an
  interrupted `backfill ch` resumes from a saved cursor ([Section 12](#12-optional-backfill-graph-enrichment)).
- `project` loads each half of the graph before wiping the old one, so an interrupted run usually
  leaves the previous projection in place. Re-run it to finish. To check that a projection really
  finished, run `cargo run -p urban-integrity-cli -- status` and look for `Projection: complete`.
  That reads `projection_runs:current.finished_at_ms`, which is set only when a run succeeds. Row
  counts can't tell you this: a partial rebuild looks exactly like a smaller finished graph
  ([Section 13](#13-project-graph-to-native-surrealdb-tables)).

### A8. Troubleshooting

See the [Troubleshooting](#troubleshooting) table at the end of this guide. The most common first-run
problems:

| Symptom | Fix |
|---------|-----|
| Build fails in `urban-integrity-api` mentioning `protoc` | Install `protobuf-compiler` (step A1) |
| Build fails in `openssl-sys` ("could not find OpenSSL" / `pkg-config`) | Install `pkg-config` and `libssl-dev` / `openssl-devel` (step A1) |
| `surreal binary not found at ~/.surrealdb/surreal` | Export `SURREAL_BIN=/path/to/surreal`, or symlink it into `~/.surrealdb/` |
| `URBAN_SURREAL_LAUNCHER=systemd but no systemd user session is reachable` | Unset `URBAN_SURREAL_LAUNCHER` (or set it to `background`) — see step A4 |
| `toy-analysis.sh`: `SurrealDB not reachable on :8000` | Start SurrealDB first (step A4) |
| `build-workbench.sh` says the `trunk` on PATH is `pg-trunk` | Install the bundler into its own root and prepend it to `PATH` (step A1) |
| Workbench page says "Workbench assets not built" | `scripts/build-workbench.sh`, then `touch crates/workbench/src/lib.rs && cargo build -p urban-integrity-cli`, restart |
| `workbench` fails immediately naming `127.0.0.1:8090` | Something already holds the port (often an earlier workbench); stop it or pass `--bind 127.0.0.1:<port>` |
| `unknown analysis '<id>'` | Pass a `db_name` from `analysis list`; omit `--analysis` to use the `config.toml` default |
| The machine becomes unresponsive during a build | Two cargo invocations at once, or no `jobs` cap — see step A0 |

---

## Part B — Reference

The rest of this guide is reference material for working beyond the toy analysis. Its numbered
sections are linked from Part A.

### Optional tools

- `cargo-llvm-cov` — coverage reports (`cargo install cargo-llvm-cov`)
- `grpcurl` — exercising the gRPC endpoints
- `cargo-flamegraph` — profiling (`cargo install flamegraph`)

**Python dependencies differ per script.** `scripts/build_deprivation_ward_index.py` uses the
standard library only. The forensic scripts in Section 11 need `requests`
(`pip install requests`). The Scrapling sidecar has its own `requirements.txt` and virtualenv
(Section 5). Nothing shares one environment; install per use.
---

## Read This First — Five Operational Rules

These five are the ones that have cost real time on this project. Everything else in this
guide is recoverable; these are not obvious, and getting them wrong either fails **silently** or
fails while naming the wrong cause.

### 1. Start SurrealDB only via `scripts/surreal-start.sh` — never `surreal start`

```bash
scripts/surreal-start.sh                 # correct
systemctl --user stop urban-surreal      # stop
```

The launcher runs SurrealDB as a transient `urban-surreal` **user systemd service** under
`user.slice`, on the RocksDB backend (`rocksdb://data/surreal-rocks.db`), with a hard cgroup
`MemoryMax` of `12G` and a soft `MemoryHigh` of `9G`. A bare `surreal start` bypasses both
protections at once: it inherits the calling shell's cgroup (in a VS Code terminal, that is the
editor's `app-code-*.scope`, so a database OOM becomes a *global* OOM that takes the editor with
it), and a `surrealkv://` path reused from shell history reintroduces the storage engine that kept
every touched key resident in RAM. This has happened repeatedly. `scripts/surreal-guard.sh` is an
**opt-in** shell function: add `source /path/to/urban-integrity/scripts/surreal-guard.sh` to your
`~/.bashrc` and it intercepts the `start` subcommand and reroutes it through the launcher; every
other `surreal` subcommand (`sql`, `import`, `export`, `version`) passes through untouched. Nothing
installs it for you. (The guard routes to the same launcher on every platform, including the
background launcher used where there is no systemd user session.)

**Run it from the repository root — never from a git worktree.** (This matters once your store
holds data you care about; on a fresh clone every store is empty anyway.) The launcher derives
`WORKSPACE_ROOT` from its own location (`scripts/surreal-start.sh` → its parent) and passes it as
the service's `WorkingDirectory`, while the store path is *relative*
(`rocksdb://data/surreal-rocks.db`). Launched from a worktree, it therefore creates and serves a
**fresh, empty per-worktree store** — and `data/` is gitignored, so nothing about the checkout hints
that this happened. `/health` returns 200, `URBAN_DB_TESTS=1 cargo test --workspace` goes green with
an identical pass count, and clippy is clean; the only signal that dissents is **duration**. The
starkest pair is the CLI test binary (`cargo test -p urban-integrity-cli --test cli`), which spawns
the CLI against whatever store the instance serves: about 450 s gated against the real store
(measured 2026-09-16, warm or freshly restarted alike) versus a couple of seconds against an empty
one. A whole-workspace gated run took 637 s wall on the same day. **A gated run that finishes in
seconds is the thing to distrust — one that takes ten minutes is normal.** Duration is still the
weakest signal (it also moves with whatever else the machine is doing), so before trusting any
result from a gated run against a populated store, confirm the instance is serving that store:

```bash
systemctl --user show urban-surreal -p WorkingDirectory   # must be the repo root
curl -s -u root:root http://127.0.0.1:8000/sql -d 'INFO FOR ROOT'   # must list urban_integrity
```

When the number matters, also count the corpus directly against the namespace and database the
tests will use — the `[surreal] database` value of the config file in effect, `config.local.toml`
if you have one, else `config.toml` (see
[Rule 2](#2-an-analysis-id-is-a-database-name-not-a-namespace)):

```bash
CFG=config.local.toml; [ -f "$CFG" ] || CFG=config.toml
DB=$(sed -nE 's/^database *= *"([^"]+)".*/\1/p' "$CFG")
curl -s -u root:root -H 'surreal-ns: urban_integrity' -H "surreal-db: $DB" \
  -X POST --data 'SELECT count() FROM graph_events GROUP ALL;' http://localhost:8000/sql
```

Recover by stopping the service and re-running the launcher **from the repo root**.

### 2. An analysis id is a **database** name, not a namespace

```
NS: urban_integrity          ← fixed for the whole install (config.toml [surreal].namespace)
DB: an_<label>_<hash>        ← varies per analysis (the --analysis / URBAN__ANALYSIS value)
```

`connect_to_db()` (`crates/cli/src/db.rs`) calls `.use_ns(namespace).use_db(db_name)` — namespace
from config, database from the resolved analysis. When you query SurrealDB by hand over HTTP or
in Surrealist, send `NS: urban_integrity` and `DB: <your analysis db_name>` (from
`analysis list`). Putting the analysis id
in the `NS` slot selects an empty namespace that SurrealDB will happily create for you, and the
query returns **zero rows with no error**. Over raw HTTP the same mistake can surface as the
thoroughly misleading `Couldn't write to a read only transaction` — read that message as "wrong
namespace", not as a permissions problem. The `_catalog` registry is likewise another *database*
under the same namespace.

### 3. If SurrealDB wedges, stop it before restarting

A wedged instance answers `/health` while queries hang (or both hang) — typically after a
concurrent bulk re-ingest has thrashed its background tasks (`Transaction conflict: Resource busy`).
`surreal-start.sh` is idempotent by design: it sees a healthy `/health` and exits without touching
anything, so it **will not replace a wedged instance**. The stop is required first:

```bash
systemctl --user stop urban-surreal
scripts/surreal-start.sh
```

This is crash-safe (RocksDB replays its WAL on restart). Observed recovery on 2026-08-20: resident
memory dropped from 9.4 GB to 527 MB and query latency returned to milliseconds.

### 4. The deprivation index does **not** ship — scoring is unweighted until you build it

`config.toml` points `[detector].imd_csv_path` at `data/wimd_2019_wards.csv`, but `.gitignore`
excludes `data/*`, so **no index arrives with a clone**. Until you generate one, `serve` logs
`IMD index load failed` and every alert carries `m_env = 1.0` with a null `imd_rank` — the
deprivation half of `P_c` is inert. Build it before your first `serve` (Step 4 below). Alerts
detected without an index are not retroactively re-weighted; there is no re-detect-all command.

**More generally, a fresh clone has an almost-empty `data/`.** Exactly three committed reference
tables survive the ignore rule, and they are the only files under `data/` that arrive with the repo:

| File | Read by |
|------|---------|
| `data/formation_agent_postcodes.txt` | `crates/detector/src/motif/cluster.rs` — compiled in with `include_str!`, so it is present whether or not the directory is |
| `data/party_authority_control.json` | `crates/enricher/src/donation_correlation.rs` — read from disk at runtime; if it is missing the donation-correlation detector logs `failed to load data/party_authority_control.json — skipping detector` |
| `data/country_incorporation_canonical.json` | `crates/enricher/src/country_canonical.rs` — also `include_str!`-compiled; canonicalises OCOD country-of-incorporation strings, so editing it requires a rebuild |

Everything else under `data/` is created on demand and never shipped: the RocksDB store
(`data/surreal-rocks.db`), graph checkpoints (`data/checkpoints`), the deprivation index, the HMLR
CCOD/OCOD/PPD download caches (`data/ccod`, `data/ocod`, `data/ppd`), and the logs the launchers
write (`data/surreal.log`, `data/serve.log`, `data/sidecar.log`). Expect the directory to grow to
tens of gigabytes once you ingest — plan the disk, and do not expect any of it back from git.

### 5. An unknown `URBAN__*` section makes the config fail to load

`AppConfig` is `#[serde(deny_unknown_fields)]`, so a typo'd or invented environment section is a
hard error, not something quietly ignored. There is **no `[db]` section** — the endpoint key is
`URBAN__SURREAL__WS_URL`:

```
$ URBAN__DB__ENDPOINT=ws://localhost:8000 urban-integrity-cli alerts list
Caused by:
    unknown field `db`, expected one of `analysis`, `surreal`, `graph`, `ingest`,
    `detector`, `enricher`, `jurisdiction`, `workbench`
```

The message names every section that *is* valid, so the fix is mechanical. Two things make it worth
a rule anyway. First, the failure is at `AppConfig::load()`, which every real subcommand calls
first — so one stray exported variable in your shell breaks *every* command at once, and the cause
is your environment rather than anything you changed in the repo. Second, `main.rs` runs the
jurisdiction preflight under `if let Ok(config) = AppConfig::load()`, so a config that will not load
**silently skips that preflight** and the nation check with it; you learn about the problem from the
subcommand, one step later than you would expect.

---

## 1. Clone & Build

The ordered steps are [A1–A3](#a1-install-the-os-prerequisites) in Part A. Details:

All 13 crates (`schema`, `core`, `ingestor`, `stats`, `detector`, `enricher`, `probe`, `api`, `cli`, `test-support`, `workbench-dto`, `workbench`, `workbench-web`) compile from a single workspace. The first build also triggers `rustup` to fetch the pinned `1.96.0` toolchain and pulls a large dependency tree; subsequent builds are incremental.

`cargo build --workspace` does **not** build the workbench's browser frontend — that is a separate, explicit Trunk step ([Section 21](#21-analyst-workbench-browser-ui)). Without it the workspace still compiles, and the `workbench` subcommand serves a "Workbench assets not built" page instead of the UI.

### Verify the build

Run the suite — no database required:

```bash
cargo test --workspace
```

To run the suite with the DB-gated integration tests actually hitting SurrealDB, start SurrealDB ([step A4](#a4-start-surrealdb)) first, then gate the run with `URBAN_DB_TESTS=1`:

```bash
URBAN_DB_TESTS=1 cargo test --workspace
```

Measured by the maintainers on 2026-09-16 with the DB gate on, against a populated store: **2,573 passed, 0 failed, 11 ignored**, 637 s wall. On a fresh clone (empty store) the gated run is much faster. A large subset of those test functions is DB-gated and needs a SurrealDB instance reachable at `ws://localhost:8000` — see [TESTING.md](TESTING.md) for the inventory and for how that count is derived. These totals move with every merge — re-measure rather than matching them:

```bash
cargo test --workspace 2>&1 | grep -E '^test result' | awk '{p+=$4; f+=$6; i+=$8} END {print p, f, i}'
```

> **The DB gate is a skip-by-return, not `#[ignore]`.** A gated test that finds `URBAN_DB_TESTS` unset returns early and is *counted as passed*, so `cargo test --workspace` and `URBAN_DB_TESTS=1 cargo test --workspace` report the **same** passing count. A green run without the variable therefore says nothing about whether the DB-gated tests exercised anything at all. This project has been bitten by exactly that before, which is why CI always sets it.

`.github/workflows/ci.yml` installs `protoc`, starts SurrealDB `v3.1.0-beta.2` via `surrealdb/setup-surreal@v2` on `:8000` with `root`/`root` and `surrealdb_auth: true`, then runs `URBAN_DB_TESTS=1 cargo test --workspace`, plus a `lint` job running `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`, and a third job that builds the workbench DTO and frontend crates for `wasm32-unknown-unknown`, runs the workbench guard scripts (`check-wasm-boundary.sh`, `check-verdict-deserialize.sh`, `check-no-analysis-literals.sh`, `check-fixture-not-shipped.sh`, `check-abbreviations.sh`, `check-shim-drift.sh`) and does a `trunk build --release`. It sets `RUSTFLAGS=-C debuginfo=0` and `RUST_TEST_THREADS=2` — worth knowing before debugging a CI-only failure.

Run the same gates locally in one shot:

```bash
scripts/check.sh          # fmt --check, clippy -D warnings, workspace tests
scripts/check.sh --fix    # apply `cargo fmt --all` first, then the gates
scripts/check.sh --db     # also run the DB-gated suite (URBAN_DB_TESTS=1)
```

`check.sh` runs **every** gate even when an earlier one fails, then prints a summary — a `fmt` failure no longer short-circuits clippy. Full logs land in `/tmp/urban-check-<timestamp>.log`.

See [docs/TESTING.md](TESTING.md) for the full test inventory and namespace-hygiene rules for new DB-gated tests.

---

## 2. Start SurrealDB

The ordered step is [A4](#a4-start-surrealdb) in Part A, which also covers hosts without a systemd
user session. Start SurrealDB with the project launcher — see [Rule 1](#1-start-surrealdb-only-via-scriptssurreal-startsh--never-surreal-start) above for why this is not optional.

```bash
# Start the isolated, memory-capped SurrealDB user service
./scripts/surreal-start.sh

# Verify it's running (HTTP 200 once ready)
curl -sf http://127.0.0.1:8000/health && echo up
```

Stop it when you're done:

```bash
systemctl --user stop urban-surreal
```

The launcher binds to `[::]:8000` (both IPv4 `127.0.0.1` and IPv6 `::1` loopback) and signs in with `--user root --pass root`, matching the defaults in `config.toml [surreal]`. It is idempotent — re-running it is a no-op if a healthy instance is already up. It also clears a stale storage `LOCK` file and kills a non-isolated `surreal` server left over from an earlier direct launch.

> **Note:** The default memory cap is `12G` (hard `MemoryMax`) / `9G` (soft `MemoryHigh`), with `MemorySwapMax=2G` and `OOMPolicy=stop`. Override the hard cap with `URBAN_SURREAL_MEM_MAX=8G ./scripts/surreal-start.sh`. Logs go to `data/surreal.log` and `journalctl --user -u urban-surreal -f`. The storage path can be overridden with `URBAN_SURREAL_DB_PATH`, and the bind address with `URBAN_SURREAL_BIND`. See [CONFIGURATION.md](CONFIGURATION.md) for the full launcher variable table.

> **Recovering a wedged instance.** The launcher will not replace a running-but-wedged server — its health probe passes and it exits early. Stop first, then start: `systemctl --user stop urban-surreal && scripts/surreal-start.sh`. See [Rule 3](#3-if-surrealdb-wedges-stop-it-before-restarting).

> **Why a launcher?** The CCOD ingest can drive the storage engine to very high RSS. Running SurrealDB under its own cgroup with a hard `MemoryMax` ensures the cgroup OOM killer reaps only SurrealDB if it exceeds the cap — the editor and the rest of the host survive. The backend was switched from `surrealkv://` to `rocksdb://` on 2026-06-17 for the same reason (RocksDB evicts to SST on disk instead of holding every touched key resident in RAM). The old surrealkv store was kept as a rollback backup until 2026-07-28, then deleted (5.6 GB) once RocksDB had been authoritative for six weeks.

**Applying the schema.** `SCHEMA_DEF` is applied idempotently by `connect_to_db()`
(`crates/cli/src/db.rs`) on *every* CLI connection, so any command that connects touches it into
place. The lightest way to initialise a fresh database is:

```bash
cargo run -p urban-integrity-cli -- status
```

It resolves `--analysis`, connects, applies `SCHEMA_DEF`, and reports what is actually there. On a
fresh clone the counts are zero and the projection reads `never run`; on a populated analysis the
output looks like this (illustrative figures from a maintainer instance):

```
Engine Status
  Database:      an_<label>_<hash>
  Graph events:  977,302

  Projection:    complete
    Nodes:         358,467
    Edges:         507,179
    Edges skipped: 99
```

> **The two blocks are different things, and the gap between them is meaningful.** `Graph events`
> is a live count of the append-only log — what has been ingested. The node and edge figures come
> from `projection_runs:current`, the marker `project` stamps, and describe **what the last
> completed projection wrote**. `project` is destructive-then-rebuild and takes over an hour on the
> live corpus, so events ingested since then are in the log and not yet in the graph. A single
> fused "node count" would hide that.
>
> `Projection` has three states, never two: `complete`, `UNFINISHED — partial or still running`,
> and `never run`. Only `finished_at_ms` distinguishes a projection that finished from one that
> died — `project` wipes before it rebuilds, so an interrupted run leaves tables that look exactly
> like a finished smaller graph. If you see anything but `complete`, do not trust a graph read.
>
> Until 2026-09-03 this command was a stub that printed four hardcoded zeros without connecting,
> and this guide recommended `alerts list` instead. Both are fine now; `alerts list` also connects
> and applies the schema.

### Fastest first run: the planted toy analysis

See [step A5](#a5-first-run-the-planted-toy-analysis-no-api-keys-no-data): `scripts/toy-analysis.sh`
needs only a running SurrealDB, touches nothing but its own fixture analysis, and prints the
`workbench` command to run next.

---

## 3. Configure

The committed `config.toml` at the repo root is a fictional example; for a real analysis copy it to the gitignored `config.local.toml`, which is then read **instead of** it (step [A7](#a7-optional-real-data)). A gitignored `.env` is also read (`dotenv::dotenv()` runs before environment variables are processed). The sections below cover each credential group.
See [CONFIGURATION.md](CONFIGURATION.md) for the full key reference — every config key lives there, not here.

> **Start from `.env.example`:** `cp .env.example .env`, then uncomment the keys you have. Keep the **double** underscore after `URBAN`: the loader is `Environment::with_prefix("URBAN").separator("__")`, so a single-underscore name (`URBAN_SURREAL__USERNAME`) is never read, with no warning and no parse error.

**Which database does the CLI talk to?** The shipped `config.toml [surreal]` section selects namespace `urban_integrity` and database `an_example_00000000` — a fictional analysis id. Every command without `--analysis` operates on that database (or on the one your `config.local.toml` names). Either point `database` in `config.local.toml` at an analysis you created, or pass `--analysis <db_name>`.

> **On a fresh clone that name is a label, not data.** The RocksDB store lives under the gitignored
> `data/`, so a new checkout starts with an empty SurrealDB. `connect_to_db()`
> (`crates/cli/src/db.rs`) selects the database and applies `SCHEMA_DEF` idempotently, and SurrealDB
> creates a database on first write — so `alerts list` succeeds against an empty graph rather than
> erroring. Every count you see in this guide comes from the maintainers' populated instance; yours
> will read zero until you ingest.
>
> **And on a fresh clone, `--analysis an_example_00000000` fails while omitting the flag works.**
> `resolve_analysis_db()` treats `None` and `main` as "use `config.toml [surreal] database`
> verbatim, no registry access", but resolves any *other* value through the `_catalog` registry —
> which is empty until an `analysis create` has run. Passing the id explicitly therefore exits with
> **unknown analysis 'an_example_00000000' — run `urban-integrity analysis list` to see known
> analyses** (the binary is actually `urban-integrity-cli`). Until you register one ([Section 20](#20-provision-and-manage-analyses)), just omit
> `--analysis`.

To work with a different analysis, use the global `--analysis` flag or the `URBAN__ANALYSIS` environment variable — see [Section 20](#20-provision-and-manage-analyses) for the analysis lifecycle (`analysis create` / `list` / `remove`). Remember [Rule 2](#2-an-analysis-id-is-a-database-name-not-a-namespace): the analysis id goes in the **database** slot.

### SurrealDB credentials — `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD`

The engine signs in to SurrealDB with the credentials set in `config.toml [surreal]`. Defaults (`username = "root"`, `password = "root"`) match the `--user root --pass root` flags the launcher passes in Step 2. Override via environment variables if your SurrealDB instance uses different credentials:

```bash
export URBAN__SURREAL__USERNAME="root"
export URBAN__SURREAL__PASSWORD="root"
```

The enricher's periodic session re-auth (every 50 minutes, applied to both the engine session and the WAL drain session, on the resolved analysis database) reads these same credentials — if they are wrong, enrichment runs log a `enricher DB re-signin failed` warning.

### The two Companies House keys are NOT interchangeable

This is the single most common setup mistake in this project. Companies House issues **two different key types**, and they are configured separately because they authorise different endpoints:

| Environment variable | Key type | Used by |
|---|---|---|
| `URBAN__INGEST__CH_API_KEY` | **Streaming API** key | `serve` — the long-lived streaming connection to `stream.companieshouse.gov.uk` |
| `URBAN__ENRICHER__CH__API_KEY` | **REST API** key | `backfill ch`, `backfill cessation`, `backfill officers`, the Rust enricher adapters (`enrich --source ch_psc`, `ch_disqualified`), and the Python CH scripts (`ch_backfill.py`, `ch_cessation_backfill.py`, `pa_link_officers.py`) |

**A REST key returns 401 on the streaming endpoint**, and the streaming key does not enable the PSC/Disqualified adapters. Set both if you want both subsystems. Register at [developer.company-information.service.gov.uk](https://developer.company-information.service.gov.uk) → create an application → request Streaming API access for the streaming key.

```bash
export URBAN__INGEST__CH_API_KEY="your-streaming-key-here"      # serve
export URBAN__ENRICHER__CH__API_KEY="your-rest-key-here"        # backfill + enricher + Python scripts
```

They can also be set in `config.toml` as `[ingest] ch_api_key` and `[enricher.ch] api_key`. An absent or empty enricher key disables the CH enricher adapters gracefully — no startup failure.

`ch_backfill.py` and `pa_link_officers.py` read `URBAN__ENRICHER__CH__API_KEY` by default and accept `--api-key` to override; they exit with `ERROR: set URBAN__ENRICHER__CH__API_KEY (the REST key) or pass --api-key` if neither is present. `ch_cessation_backfill.py` has no `--api-key` flag — it reads the environment variable only and exits with `URBAN__ENRICHER__CH__API_KEY not set (REST key, not streaming)`.

### Land Registry API key — `URBAN__ENRICHER__LAND_REGISTRY__API_KEY`

Used by the CCOD (Corporate Ownership of Land: Domestic) adapter. Requires registration at [use-land-property-data.service.gov.uk](https://use-land-property-data.service.gov.uk). An absent or empty value disables the CCOD adapter gracefully. The same HMLR account also licenses the OCOD (Overseas Companies) dataset — set `URBAN__ENRICHER__OCOD__API_KEY` separately, or leave it blank to inherit this key.

```bash
export URBAN__ENRICHER__LAND_REGISTRY__API_KEY="your-hmlr-api-key-here"
```

Set in `config.toml` as `[enricher.land_registry] api_key` or via the environment variable above.
Neither cache ships with a clone: the first enabled run downloads the bulk ZIPs into
`[enricher.land_registry] cache_dir` (`data/ccod`) and `[enricher.ocod] cache_dir` (`data/ocod`), and
re-downloads them every `ccod_refresh_interval_days` / `ocod_refresh_interval_days` (30 by default,
matching HMLR's monthly publication). These are large, multi-gigabyte-once-expanded datasets — the
CCOD ingest is the reason SurrealDB runs under a memory cap ([Rule 1](#1-start-surrealdb-only-via-scriptssurreal-startsh--never-surreal-start)).

All other defaults in the example `config.toml` work out of the box for development.

---

## 4. Build the Deprivation Index

`data/*` is gitignored, so the deprivation index the detector joins to **does not arrive with a
clone** — see [Rule 4](#4-the-deprivation-index-does-not-ship--scoring-is-unweighted-until-you-build-it).
Generate it before your first `serve`:

```bash
# One Welsh local authority (replace with your authority's W06… code)
python3 scripts/build_deprivation_ward_index.py --nation wales --lad <W06… code>

# Every ward Wales publishes (supported for wales and northern-ireland only)
python3 scripts/build_deprivation_ward_index.py --nation wales --all

# England / Scotland must name their local authorities explicitly
python3 scripts/build_deprivation_ward_index.py --nation england --lad <E06…/E07…/E08…/E09… code>
```

`--nation` is mandatory. For `wales`, `england` and `scotland` you must also pass `--lad <ONS local-authority codes>` **or** `--all`; the script exits with an error otherwise, because `--all` for England or Scotland would page the entire national postcode directory. `--all` itself is rejected for `england` and `scotland`. `northern-ireland` needs neither — NISRA ranks wards directly (and is the one nation that needs LibreOffice, because NIMDM ships only as legacy `.xls`). `--out` overrides the destination path.

> **Older notes may show a bare `--nation wales` or an `--all-wales` flag.** Neither is accepted:
> the bare form exits with `error: --nation wales needs the local authorities to cover: use --all
> for every Welsh ward, or --lad <codes> to scope it.`, and `--all-wales` is not a recognised
> argument. `config.toml`'s `[detector]` comment shows the current forms, matching this section.

**One nation per file.** The four UK deprivation indices are not comparable, and each writes its own filename under `data/`:

| `--nation` | Index | Denominator | Output file | Ward prefix |
|-----------|-------|-------------|-------------|-------------|
| `wales` | WIMD 2019 | 1,909 | `data/wimd_2019_wards.csv` | `W05` |
| `england` | IMD 2019 | 32,844 | `data/imd_2019_wards.csv` | `E05` |
| `scotland` | SIMD 2020v2 | 6,976 | `data/simd_2020_wards.csv` | `S13` |
| `northern-ireland` | NIMDM 2017 (Ward2014) | 462 | `data/nimdm_2017_wards.csv` | `N08` |

Point `[detector].imd_csv_path` in your `config.local.toml` at the file matching your analysis's
nation. The example `config.toml` sets `data/wimd_2019_wards.csv` purely as an example value.

> **A startup jurisdiction preflight refuses a mismatch.** `main.rs` derives the nation from the configured ward codes (the ONS GSS prefix names it) and refuses to start if the deprivation index belongs to a different nation, or if the wards span two. It refuses rather than warns because the wrong nation's index loads cleanly, matches zero wards, and leaves the weighting inert while appearing configured. The loader rejects a file whose rows disagree about the `IMD Denominator` column; if the column is absent it still loads but logs a `WARN`, because `max_rank` is then inferred from the file and a regional extract would normalise against its own worst ward.

Verify the index is being read: on `serve` startup, the absence of an `IMD index load failed` line
means it loaded.

---

## 5. (Optional) Start the Scrapling Sidecar

Scrapling handles planning-portal scraping. Skip this step if you only need Companies House + Contracts Finder data. The sidecar is a **separate** FastAPI process — not part of the Cargo workspace.

Use the launcher rather than starting `sidecar.py` by hand — it runs the sidecar as a transient `urban-sidecar` user systemd service (detached from your terminal, falling back to `nohup` where `systemd-run --user` is unavailable) and is idempotent:

```bash
./scripts/sidecar-start.sh
```

Verify it's running:

```bash
curl http://localhost:8765/health
# {"status":"ok","service":"scrapling-sidecar"}
```

Stop it with `systemctl --user stop urban-sidecar` (or `kill "$(cat data/sidecar.pid)"`). Logs go to `journalctl --user -u urban-sidecar -f` and `data/sidecar.log`. Tune with `SIDECAR_PORT` and `URBAN_SIDECAR_PYTHON`.

The launcher prefers the sidecar's own virtualenv at `scrapling-sidecar/.venv/bin/python` and falls back to system `python3`. To create it and install dependencies the first time:

```bash
python3 -m venv scrapling-sidecar/.venv
scrapling-sidecar/.venv/bin/pip install -r scrapling-sidecar/requirements.txt
```

`requirements.txt` pins `fastapi>=0.115.0`, `uvicorn[standard]>=0.30.0`, `scrapling>=0.3.0`, `firecrawl-py>=1.0.0` and `pydantic>=2.0.0`. `playwright` is listed but commented out — it is only needed for JS-rendered portals, not for the Idox or Arcus strategies.

Ingestion degrades gracefully with a `WARN` log if the sidecar is down — the engine does not fail.

---

## 6. Start the Engine

```bash
# Start graph engine + ingestors + API server
cargo run -p urban-integrity-cli -- serve
```

The engine will:
1. Connect to SurrealDB, apply the schema, and replay existing `graph_events` into the in-memory coordinator so detectors see historical data (if events exist and replay fails, `serve` refuses to start rather than run blind on an empty graph)
2. Start the Companies House stream (requires the **streaming** key `URBAN__INGEST__CH_API_KEY`)
3. Start the Contracts Finder poller (`cf_poll_interval_secs`, default 900s / 15 min)
4. **Start the planning portal poller** if `[[ingest.planning_portals]]` entries are configured (polls each ward at `planning_poll_interval_secs` — default 3600s / 1 hr). The example `config.toml` has its entries commented out, so the poller stays off until your `config.local.toml` configures real wards
5. Run the motif detectors — `serve` is the **only** process that does. Its `DetectorConfig` is read from the `[detector]` block of `config.toml` (`phoenix_window_days`, `phoenix_min_value`, `imd_csv_path`); an absent block falls back to a 180-day window, never wider
6. Expose HTTP/GraphQL on `:8080` and gRPC on `:8081`
7. Run the background enricher on the configured interval, and re-authenticate the SurrealDB session every 50 minutes against the ~1-hour token expiry

Log output is structured (JSON + `RUST_LOG` filter) via `tracing-subscriber`. Set `RUST_LOG=urban_integrity=debug` for verbose output.

> **Watch the startup log for `IMD index load failed`.** If it appears, Step 4 has not been completed (or `imd_csv_path` points at the wrong nation's file) and every alert this run produces will carry `m_env = 1.0` and a null `imd_rank`.

---

## 7. Generate Your First Report

The `report` subcommand takes filter flags (`--ward`, `--min-score`, `--motif`, `--from`/`--to`, `--entity`/`--scope`) and an output format. The format flag is `--format` (values: `table`, `json`, `md`, `pdf`); `--output` is accepted as a hidden alias for backwards compatibility. `--output-file` is only valid with `--format md` or `--format pdf`.

```bash
# All alerts above 0.7 score
cargo run -p urban-integrity-cli -- report --min-score 0.7

# Filtered by ward code
cargo run -p urban-integrity-cli -- report --ward X05999001 --min-score 0.5

# JSON output (machine-readable)
cargo run -p urban-integrity-cli -- report --format json | jq .

# Entity-scoped report across all LPAs
cargo run -p urban-integrity-cli -- report --entity "Apex Build Ltd" --scope all-lpa

# Write a Markdown report to a file
cargo run -p urban-integrity-cli -- report --format md --output-file report.md
```

Withdrawn alerts are excluded from reports (see Section 8). A truncated report prints `showing N of M matching alerts` rather than leaving the omission to be inferred.

> **`report --motif` does not normalise the spelling; `alerts list --motif` does.** `alerts list` runs its argument through `normalize_motif_type` (`crates/cli/src/commands/alerts.rs`), which maps `phoenix` → `PhoenixLoop`, `cluster` → `AddressCluster`, `nepotism` → `NepotismBridge`. `report` passes the raw argument straight into the filter (`crates/cli/src/commands/report.rs`), so `report --motif phoenix` matches nothing while `report --motif PhoenixLoop` matches. GraphQL and gRPC likewise take the exact variant name.

> **What the maintainers' reference dataset holds** (a fresh clone shows an empty report until you ingest)**.** On the maintainers' Anglesey analysis, `AddressCluster` carries essentially all the volume; `PhoenixLoop` holds two alerts (one withdrawn as a name-only officer merge, one still standing) and **`NepotismBridge` holds zero**, so `--motif nepotism` returns nothing there even though the spelling normalises correctly. NepotismBridge has been exercised on a Sandwell analysis that is not part of the maintainers' current catalogue — see the README note. All of these are flagged patterns for review, not findings of wrongdoing. An empty Phoenix section on a fresh graph is the data, not a broken install: the CH stream only emits companies that *change*, so the graph's dissolved-company universe is effectively one bulk-dissolution day wide until you widen it with `backfill cessation`.

Alternatively, `scripts/report.sh` wraps `cargo run -p urban-integrity-cli -- report` and drives the
whole cycle for you: it starts SurrealDB (via `scripts/surreal-start.sh`), builds and backgrounds
`serve` with its output going to `data/serve.log`, waits 5 s, runs the report, and then **stops both
again** — an `EXIT`/`INT`/`TERM` trap kills the `serve` process and runs `systemctl --user stop
urban-surreal`. Do not use it as a way to leave the engine up; start `serve` yourself for that.

```bash
./scripts/report.sh --ward X05999001 --min-score 0.7
```

Its argument surface is narrower than the subcommand's — `--ward` (**required**; the script prints
usage and exits 1 without it), `--min-score` (default `0.7`) and `--output table|json`. No `--motif`,
no `--from`/`--to`, no Markdown or PDF output. Two fresh-clone snags: it resolves the SurrealDB binary
as `~/.surrealdb/surreal` **directly and does not honour `SURREAL_BIN`** (unlike `surreal-start.sh`),
dying with `SurrealDB not found at …` if it is installed elsewhere; and it warns — then continues —
when `URBAN__INGEST__CH_API_KEY` is unset, so on a fresh clone expect the run to complete against
whatever is already in the database rather than to ingest anything.

---

## 8. Explore Alerts

The `alerts` subcommand is a group with `list`, `show`, and `withdraw`.

```bash
# List recent alerts (min-score defaults to 0.5; --motif, --ward, --since filters available)
cargo run -p urban-integrity-cli -- alerts list

# List high-score nepotism alerts in the last 7 days
cargo run -p urban-integrity-cli -- alerts list --motif nepotism --min-score 0.7 --since 7d

# Show full detail for a specific alert
cargo run -p urban-integrity-cli -- alerts show <alert-id>

# Withdraw an alert a data correction has invalidated (--reason is mandatory and stored)
cargo run -p urban-integrity-cli -- alerts withdraw <alert-id> --reason "rests on a name-only officer merge"

# Include withdrawn alerts, which are hidden by default
cargo run -p urban-integrity-cli -- alerts list --include-withdrawn

# Event-log count plus the last projection's node/edge figures and its completion state
cargo run -p urban-integrity-cli -- status
```

**Ordering is by score descending**, with `detected_at` then id as tie-breaks, and filters are applied over the **whole** `MotifDetected` corpus rather than inside a recency window. A truncated page prints `showing N of M matching alerts, highest score first`. All three of those were defects until 2026-08-18, and between them they hid the only live Phoenix alert this project has produced — if you are reading an older note that says a motif holds nothing, re-check it.

`alerts withdraw` **marks** rather than deletes: a retracted false positive is evidence about detector quality. Withdrawn alerts are hidden by default on every publishing surface (`alerts list`, `report`, GraphQL, gRPC). Re-withdrawing an already-withdrawn alert is refused rather than silently overwriting the original reason. A withdrawal is not permanent suppression — if the motif matches again, the upsert clears it, which is correct, because it is then a live finding again.

Note: `--since` conflicts with `--from`/`--to` — use one or the other.

> **Sanity-check the graph itself, not just the alerts.** Two read-only commands are worth running
> early on any analysis you did not build yourself:
>
> ```bash
> cargo run -p urban-integrity-cli -- probe                 # verdict-gated battery, writes evidence-report.md
> cargo run -p urban-integrity-cli -- probe --full-battery  # the same 22 probes, ungated
> cargo run -p urban-integrity-cli -- probe-oracle          # ungated battery + an independent population count
> ```
>
> **`probe` gates by default**: a probe whose upstream dependency found nothing is reported
> `SKIPPED` rather than run, so on a sparse graph most of the battery will not execute. Pass
> `--full-battery` for the complete run — see [Section 11](#the-two-probe-batteries).
>
> `probe-oracle` re-derives each checkable probe's population straight from `graph_events` by a code
> path that shares nothing with the probes, and prints the declared and recomputed counts on the same
> row — so a probe measuring the wrong population shows up as a disagreement instead of as a
> confident number nobody checked. See [Section 11](#cross-check-the-battery-probe-oracle).

---

## 9. Review Data Anomalies

An anomaly is **not** an alert. An alert says *"this looks like corruption"*; an anomaly says
*"this input could not be trusted, and here is exactly what was skipped"*. They live in separate
tables so data-quality noise never enters an analyst's corruption queue.

```bash
# Open anomalies, highest severity first
cargo run -p urban-integrity-cli -- anomalies list

# One kind only, or include already-adjudicated rows
cargo run -p urban-integrity-cli -- anomalies list --kind officer_key_collision
cargo run -p urban-integrity-cli -- anomalies list --all

# Full evidence for one anomaly
cargo run -p urban-integrity-cli -- anomalies show <anomaly-id>

# Adjudicate: accept it, or record it as not worth acting on
cargo run -p urban-integrity-cli -- anomalies review <anomaly-id>
cargo run -p urban-integrity-cli -- anomalies review <anomaly-id> --dismiss
```

Rows live in the `data_anomalies` table. Anomaly ids legitimately contain colons (they are built
from the source and the subject, e.g. `officer_key_collision_backfill:officers:audit_director:…`),
so pass the id exactly as `anomalies list` prints it.

> **Known gap:** `data_anomalies` has no retraction path. An audit that stops reporting a finding leaves the old `open` row in place indefinitely.

---

## 10. Scrape Planning Portal Data (On-Demand)

With the Scrapling sidecar running (Step 5) and wards configured in `config.toml`, scrape planning application data immediately (without waiting for the background poller):

```bash
# Scrape a single ward
cargo run -p urban-integrity-cli -- ingest-planning --ward X05999001

# Scrape all configured wards
cargo run -p urban-integrity-cli -- ingest-planning --all
```

Output:
```
╔══════════════╦══════════════╦═════════╦═════════╦═════════════════════╗
║ Ward Code    ║ Applications ║ Written ║ Skipped ║ Status              ║
╠══════════════╬══════════════╬═════════╬═════════╬═════════════════════╣
║ X05999001    ║ 12           ║ 84      ║ 0       ║ ✓ ok                ║
╚══════════════╩══════════════╩═════════╩═════════╩═════════════════════╝

Total: 1 ward(s), 12 applications, 84 events written, 0 skipped (already in DB)
```

If the sidecar is unreachable, the command exits cleanly with a warning. Configure wards in the `[[ingest.planning_portals]]` array of your `config.local.toml` — the example `config.toml` only carries commented-out, fictional entries. A ward re-list is one sidecar request covering the whole ward, so `scrapling_timeout_secs` (default 300) must clear the slowest ward. See [CONFIGURATION.md](CONFIGURATION.md).

---

## 11. Run the Forensic Investigation Pipeline

After ingesting data, run the pipeline in order to enrich the graph and run the forensic probe. The Python scripts require Python 3.11+ with `requests` installed (`pip install requests`), and SurrealDB must be running at `localhost:8000` (start it with `./scripts/surreal-start.sh`).

### Selecting the target analysis first

The Python scripts do **not** default to any database. `scripts/surreal_target.py` resolves the target with the same precedence the Rust CLI uses:

```
--analysis <db_name>  >  $URBAN__ANALYSIS  >  $URBAN__SURREAL__DATABASE  >  config.toml [surreal].database
```

There is deliberately no fallback default: SurrealDB creates a database on first write, so a script aimed at a name that does not exist would quietly populate a fresh empty one, find nothing, and report success. A preflight refuses to run unless the target database already exists, and names the available alternatives when it does not. `analyst_probe.py` honours the same environment-variable precedence but exposes **no** `--analysis` flag — select its target with `URBAN__ANALYSIS`.

```bash
ANALYSIS=an_<label>_<hash>    # your analysis's db_name from `analysis list`; must already exist
export URBAN__ENRICHER__CH__API_KEY="your-rest-key"   # the REST key, not the streaming key
```

The Rust `--analysis` flag in the steps below also needs that analysis to be **registered in
`_catalog`**, not merely to exist as a database — a fresh clone's registry is empty, so those
commands exit with `unknown analysis`. Either register it with `analysis create`
([Section 20](#20-provision-and-manage-analyses)), or drop the `--analysis` flag so the CLI uses
`config.toml [surreal] database` directly. The Python scripts resolve their target differently (the
precedence above) and do not consult the registry, but they do require the database to exist
already.

### The order matters

```bash
# 1. Ingest councillors as PoliticalActor nodes (pa_ingest.py scrapes the
#    Sandwell ModernGov member index — it is Sandwell-specific)
python3 scripts/pa_ingest.py --analysis "$ANALYSIS"

# 2. Resolve planning applicants to Companies House entities (exact identifier match)
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" backfill ch --dry-run
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" backfill ch

# 3. Backfill officers for CH entities that have none. This is the ONLY path by which
#    officer nodes reach the graph — the CH company-profile stream never emits
#    appointments. Resumable; a full pass is long-running.
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" backfill officers --dry-run
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" backfill officers

# 4. Fix edge directions for correct traversal
python3 scripts/fix_edge_directions.py --analysis "$ANALYSIS"

# 5. Audit the officer keys already in the graph, and split the uncorroborated collisions
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" backfill officers --audit
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" backfill officers --split-collisions --dry-run

# 6. Link councillors to Companies House officer records (nepotism bridge)
python3 scripts/pa_link_officers.py --analysis "$ANALYSIS"

# 7. Run OSINT enrichment. See Section 18 for the full adapter table.
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" enrich --source all

# 8. Run the probe battery in Rust (writes evidence-report.md). Verdict-gated by
#    default; add --full-battery to run all 22 regardless of upstream verdicts.
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" probe

# 9. Cross-check every checkable probe's declared population against an
#    independently computed graph_events count
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" probe-oracle

# 10. Run the Python battery for a second opinion on the same graph
URBAN__ANALYSIS="$ANALYSIS" python3 scripts/analyst_probe.py
```

> ### ⚠️ Run the officer backfill (step 3) BEFORE `enrich --source ch_psc`
>
> The PSC adapter **pre-loads the full set of officer ids once**, at the start of its run, and then
> matches every fetched PSC record against that in-memory set. If the officer layer is empty when the
> adapter starts, every PSC lookup misses and the adapter writes **zero identity links** — while still
> reporting a successful run with records fetched. Nothing errors, and nothing in the output tells you
> the join was vacuous.
>
> This has silently cost real runs. Always complete `backfill officers` (and the collision repair in
> step 5) before `enrich --source ch_psc` or `enrich --source all`.
>
> If you have already run PSC out of order, you do **not** need to re-fetch every PSC record: run
> `cargo run -p urban-integrity-cli -- backfill psc-links` to re-run the PSC↔officer join over data
> already staged in the graph, with no CH API calls. It shares the `ch_psc` adapter's own matcher and
> record builder, so the two paths cannot diverge, and it is idempotent — existing `ch_psc` links are
> skipped, not duplicated.

**Output:** both batteries write `evidence-report.md` to the current working directory, with a per-probe verdict on each row. The Rust `probe` takes `--output <PATH>` to write the Markdown elsewhere and `--json <PATH>` to dump the raw 22-`Finding` battery as JSON (the two flags are independent; a `--json`-only run writes no Markdown). Both create the parent directory if it does not exist. On the Python side, `python3 scripts/analyst_probe.py --no-report` prints to stdout only. **They write the same filename — run them in different directories, or pass `probe --output` — or the second run silently overwrites the first.**

### The two probe batteries

`urban-integrity-cli probe` (step 8) is the **Rust battery**, and it now registers all **22 probes** —
the port is complete. Registration order is fixed and load-bearing: the 7 graph probes
(`precomputed-alerts`, `identity-integrity`, `detector-readiness`, `address-cluster-exact`,
`geographic-concentration`, `ward-concentration`, `temporal-burst`), then the 12 OSINT probes in
`analyst_probe.py`'s own `OSINT_PROBES` order (`identity-link-registry` first — it writes the
strategy fields every later OSINT probe reads), then the 3 anchor probes `land-flip-candidates`,
`donation-contract-correlation` and `opencorporates-cross-jurisdiction`. Select the target with the
global `--analysis` (which it *does* honour, unlike `export-graphml --source events`); the
subcommand's own flags are `--output`, `--json` and `--full-battery`.

#### Dispatch is verdict-gated by default

**All 22 probes no longer run on every dispatch.** `probe` dispatches through a gate predicate, and
a probe the gate excludes is reported `SKIPPED` without its `run()` ever being called. Two rules
decide it, evaluated against the verdicts already accumulated *this run*:

- **Declared dependencies (AND).** Every dependency a probe names must have returned `HIT` or
  `WEAK`. The mechanism lives in `crates/probe/src/graph.rs`, and the shipped battery currently
  declares **no** dependency edges — a unit test there fails if one is added. The single edge the
  battery first shipped with (`address-cluster-exact` → `identity-integrity`) was removed on
  2026-09-03: `identity-integrity` can never return `HIT` or `WEAK`, so the gate could never open
  and `address-cluster-exact` was silently skipped on every default run. Should edges return, the
  graph is topologically ordered, so a dependency is always dispatched before its dependent.
- **The Tier-1 → Tier-2 OR rule.** The 7 Tier-1 probes are the graph probes listed above; the other
  **15 are Tier-2**. A Tier-2 probe runs only if *at least one* Tier-1 probe returned `HIT` or
  `WEAK`. On a graph where every Tier-1 probe comes back `CLEAR`, `DATA_GAP` or `BLOCKED` — which
  is the normal shape of a fresh or lightly-ingested analysis — **all 15 Tier-2 probes are skipped
  in one go**, and the run finishes in seconds with 15 `SKIPPED` rows. With no dependency edges
  declared, this rule is the only thing that gates today.

```bash
# The complete, ungated 22-probe run — always available for comparison against a gated one
cargo run -p urban-integrity-cli -- probe --full-battery
```

Two things gating never bypasses: the dependency graph is validated (acyclic, no tier
interleaving) **before** either dispatch mode runs, and `--full-battery` still goes through that
validation. A gated-out probe is given no population provenance, because it measured nothing.

Its verdict vocabulary has **seven** values — `HIT`, `WEAK`, `CLEAR`, `NOT_APPLICABLE`, `DATA_GAP`,
`BLOCKED`, `SKIPPED`. The first four are rendered with the `population/capable` coverage pair they
were drawn from; `SKIPPED` instead names the cause that excluded the probe, either
`upstream '<probe>' returned <VERDICT>` or `no Tier-1 probe (of N checked) returned Hit or Weak`,
so a skip can never point at something that did not actually gate it (the first form can only
appear if a dependency edge is declared). A
`CLEAR` can only be constructed when the structurally-capable subset covers the whole population, so
the battery cannot report "nothing suspicious found" about a population it never measured.
`precomputed-alerts` reads the `detector_runs:current` marker before reporting, so zero alerts is
distinguishable from a detector that never ran — expect `DATA_GAP` on any analysis that has not run
`serve` since that marker shipped.

`scripts/analyst_probe.py` (step 10) is the original Python battery. It has **none** of those
verdict fixes — read its `CLEAR` verdicts with that in mind — but it is still the independent second
opinion the Rust port was checked against, so keep running both while the two are being reconciled.

### Cross-check the battery: `probe-oracle`

`urban-integrity-cli probe-oracle` (step 9) runs the same 22-probe battery — **always ungated**, as if
`--full-battery` had been passed, since a gated-out Tier-2 probe would declare no population and
show up as an empty comparison row rather than as a checked one — and then recomputes each
checkable probe's declared population **independently**, straight from `graph_events` via
`crates/core/src/persistence/oracle.rs`. That module deliberately shares no implementation with the
`persistence::scan` layer the probes themselves read through — a structural test
(`crates/core/tests/oracle_independence_guard.rs`) fails if `oracle.rs` ever gains a `scan::` import
or call. The point of the separation is that a defect on either side surfaces as a *disagreement*
rather than as two code paths agreeing on the same wrong number.

```bash
# Print both tables to stdout
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" probe-oracle

# Also write a plain-text rendering of both tables to a file
cargo run -p urban-integrity-cli -- --analysis "$ANALYSIS" probe-oracle --output oracle-report.txt
```

It prints two tables:

| Table | Columns | Contents |
|-------|---------|----------|
| Comparison | `Probe`, `Declared`, `Oracle`, `Delta`, `Agrees` | The **10** probes whose population can be independently recomputed, declared and oracle counts on the *same* row: `precomputed-alerts`, `identity-integrity`, `address-cluster-exact`, `geographic-concentration`, `ward-concentration`, `temporal-burst`, `gazette-insolvency-signals`, `property-nodes`, `osint-new-node-types`, `phase20-council-registers` |
| Coverage / exclusions | `Probe`, `Category`, `Reason` | The **12** probes that cannot be checked this way, each named with a specific, source-cited reason — never a blanket "Tier-2" label |

The four exclusion categories are `IdentityLinksSourced` (the population comes from
`identity_links`, not `graph_events`), `VerdictThreshold` (the population is a hardcoded classify
threshold, not a real count), `SemanticMismatch` (it counts something structurally different from an
entity or edge count) and `MarkerArithmetic` (it is arithmetic over scalar fields of one marker
record).

Reading the output:

- **`Delta` is 0 for every probe today** — the allowed-delta table is empty, so any difference at
  all is a disagreement.
- **A row reading `not measured this run`** means that probe's verdict was `DATA_GAP` or `BLOCKED`,
  so it declared no population. It is not compared against 0, which would be a false disagreement.
- **A disagreement is informational — the command still exits 0.** Investigate it; do not adjust
  either side to force agreement. `geographic-concentration` disagreed for a real reason
  (102,897 declared vs 61,756 oracle on the maintainer's analysis, 2026-09-01): the probe scanned one
  row per `NodeAdded` *event* instead of one per *entity*, so re-ingested Locations were
  double-counted. Fixed in `c8fb0c7`; the check stays in place as the regression guard.
- **`address-cluster-exact` has a known counting-shape asymmetry** — the probe requires both edge
  endpoints to parse, the oracle counts distinct destinations regardless. A small `oracle > declared`
  gap there is an artifact of the two shapes, not a new finding. The source comment in
  `crates/cli/src/commands/probe_oracle.rs` says so at the check itself.
- **`--output` artifacts are plain text.** The file is rendered from de-styled copies of the two
  tables, so it carries no ANSI escape codes even when you run from a terminal — it can be diffed
  directly.
- **The coverage invariant is enforced at runtime** (it was a `debug_assert!`, compiled out of
  release, until 2026-09-01). A probe drifting out of both tables now makes `probe-oracle` fail
  with a named error rather than render an incomplete coverage statement.
- **Repointing the analysis mid-run is a hard error.** The battery is a long window on a live
  corpus (minutes, not seconds), so `probe-oracle` resolves the analysis
  database before it starts and re-resolves it afterwards. If an `analysis` retire or repoint moved
  it in between, the command aborts rather than compare the oracle's queries against a different
  database than the probes read — every row would otherwise be a fabricated disagreement.

> **On query-shape safety.** `oracle.rs` used to assert immunity to the SurrealDB index-planner bug
> because it issues only `count()`/`GROUP BY` queries. That claim was withdrawn on 2026-09-01:
> two of the three functions wrap a row-materialising inner `SELECT` and are now explicitly
> allowlisted, on evidence that is a live cross-check — a sample, not a proof. Good enough to trust
> the numbers on this corpus; re-run the cross-check before trusting them on another.

> **Exact-match only (2026-06-13):** All in-app fuzzy / phonetic name matching was removed
> in favour of exact-identifier joins. Identity links and CH backfills are created from
> registration numbers and exact normalised-name equality only — there are no
> similarity-threshold or fuzzy-tuning knobs to configure. If a backfill or enrichment step
> produces fewer links than expected, the cause is missing exact identifiers in the data, not
> a threshold setting.

**Prerequisites by step:**
- Step 1 (`pa_ingest.py`) scrapes the Sandwell ModernGov member index — it is Sandwell-specific.
- Steps 2, 3, 5 and 6 call the CH REST API and require **`URBAN__ENRICHER__CH__API_KEY`** (the REST key). The streaming key will 401.
- Step 7 CH adapters (`ch_psc`, `ch_disqualified`) require the same REST key.
- Step 7 Land Registry CCOD/OCOD adapters require `URBAN__ENRICHER__LAND_REGISTRY__API_KEY`.
- Step 7 keyless adapters (London Gazette, Charity Commission, ICIJ, Electoral Commission, PPD, FCA Mutuals, planning identity) use public data — no key needed.
- **Check which dry-run convention a script uses before running it.** The five Python pipeline scripts take `--dry-run` to *preview* (they write by default), while `ch_cessation_backfill.py`, `resolve_planning_applicants_to_ch.py`, `cf_backfill_dates.py`, `rekey_officers_to_director_slug.py` and `split_false_merged_officers.py` are **dry-run by default** and need `--apply` to write. `officer_merge_audit.py` is read-only and never writes (its only flag is `--refresh`). The `backfill` CLI subcommands all take `--dry-run` to preview.

```bash
# Safe inspection — no writes
python3 scripts/pa_ingest.py              --analysis "$ANALYSIS" --dry-run
python3 scripts/fix_edge_directions.py    --analysis "$ANALYSIS" --dry-run
python3 scripts/normalize_officer_keys.py --analysis "$ANALYSIS" --dry-run
python3 scripts/pa_link_officers.py       --analysis "$ANALYSIS" --dry-run
```

### Other forensic scripts

| Script | Purpose |
|--------|---------|
| `ch_officer_coverage.py` | Close Phoenix's real gap — Contracts Finder awardee companies that carry no officers at all |
| `phoenix_funnel.py` | Measure where Phoenix candidates die, against `graph_events` rather than the projected tables |
| `build_deprivation_ward_index.py` | Build the ward-level deprivation index for one UK nation (Step 4) |
| `projection_audit.py` | Classify each projected table as TRUNCATED, STALE, DROPPED or COMPLETE against `graph_events` |
| `ch_cessation_backfill.py` | Widen Phoenix's failed-company coverage (superseded by `backfill cessation`) |
| `cf_backfill_dates.py` | Backfill `date_published` onto Contracts Finder contracts already in the graph, from OCDS `release.date` (the mapper fix is forward-only) |
| `resolve_planning_applicants_to_ch.py` | Resolve free-text planning applicants against Companies House to give them a real identifier |
| `corroborate_offsite_applicant_matches.py` | Clear applicant→company matches withheld for review, using a second corroborating signal |
| `repair_psc_officer_links.py` | Repoint the `ch_psc` identity links whose officer target node does not exist |
| `rekey_officers_to_director_slug.py` | Re-key officer nodes from per-company `OFF:{company}-{NAME}` to the canonical `director:{slug}` |
| `officer_merge_audit.py` | Adjudicate the name-only officer fusions the re-key could not distinguish |
| `split_false_merged_officers.py` | Split back apart officer nodes the name-only re-key wrongly fused |
| `normalize_officer_keys.py` | Normalise officer `EntityId` slugs to the cross-LPA `director:{slug}` dedup invariant |
| `land_flip_funnel.py` | Measure the land-flip funnel end to end against live data, using the shipped gate-1 |
| `land_flip_pairable.py` | Report which planning applications are even pairable with a staged PPD transaction |
| `prune_isolated_nodes.py` | Streaming two-pass removal of degree-zero nodes from a GraphML export |
| `rebuild-anglesey.sh` | Maintainer-specific: refresh the maintainers' reference analysis **without** re-fetching external data (takes `--analysis`) |
| `surreal-drop-test-ns.sh` | Sweep `test*` namespaces stranded by SIGKILLed test runs (dry-run by default; `--apply` to remove) |
| `status.sh` | Read-only "where are we?" snapshot — recent commits, uncommitted changes, worklog tail |

> **Tip:** `scripts/enrich.sh` wraps step 7 (`cargo run -p urban-integrity-cli -- enrich`) and
> manages starting SurrealDB for you (via `scripts/surreal-start.sh`), stopping the `urban-surreal`
> unit on exit. Pass `--no-db` if SurrealDB is already running, and `--source <name>` to run a single
> adapter. The London Gazette adapter backfills from its `lookback_days` window on its first run;
> subsequent runs are incremental.

---

## 12. (Optional) Backfill Graph Enrichment

Before projecting the graph into native tables (Section 13), you can run backfill commands to enrich the graph data already stored in `graph_events`. `backfill` is a subcommand group with six modes: `ch`, `ward`, `planning`, `cessation`, `officers`, `psc-links`.

```bash
# Link planning applicants to Companies House numbers (requires the CH REST key)
cargo run -p urban-integrity-cli -- backfill ch

# CH REST rate limit — default 2 req/s. CH allows 600 requests / 5 minutes and
# throttles above that; the previous default of 5/s produced 429s within a minute.
cargo run -p urban-integrity-cli -- backfill ch --rate-limit 2

# Discard the saved resume cursor and walk every candidate again
cargo run -p urban-integrity-cli -- backfill ch --restart

# Resolve ward codes via postcodes.io (no API key needed)
cargo run -p urban-integrity-cli -- backfill ward

# Re-scrape the wards still attributed to the unknown-applicant placeholder and
# stamp explicit applicant provenance across the planning corpus
cargo run -p urban-integrity-cli -- backfill planning

# Fetch real `date_of_cessation` from CH, so Phoenix's window is anchored where the
# company actually failed rather than where the graph first observed it
cargo run -p urban-integrity-cli -- backfill cessation --dry-run
cargo run -p urban-integrity-cli -- backfill cessation --rate-limit 2 --max-fetches 500

# Fetch officers for CH entities that have none, keyed on the identity evidence CH
# publishes (officer id, {month, year} date of birth) rather than on the name alone
cargo run -p urban-integrity-cli -- backfill officers --dry-run
cargo run -p urban-integrity-cli -- backfill officers --audit
cargo run -p urban-integrity-cli -- backfill officers --split-collisions --dry-run

# Re-run the PSC↔officer identity join over data already in the graph — no CH API calls
cargo run -p urban-integrity-cli -- backfill psc-links
```

`backfill ch`, `cessation` and `officers` use `URBAN__ENRICHER__CH__API_KEY` — the **REST** key, not the streaming key — from your environment or `config.toml`; if the key is absent or empty, the command exits immediately with `Companies House REST key not configured — set URBAN__ENRICHER__CH__API_KEY`. All three default to `--rate-limit 2` because CH allows 600 requests per 5 minutes and throttles above it. A full `backfill ch` pass is roughly 20,000 lookups at 2 req/s (several hours), so an interrupted run resumes from a saved cursor by default; `--restart` discards it.

`backfill officers --split-collisions` **rewrites `graph_events`** — it is a data correction. Run it with `--dry-run` first, and `project` afterwards. `--audit` and `--split-collisions` are mutually exclusive.

`backfill ward` calls the free [postcodes.io](https://postcodes.io) API and requires no credentials. All six accept `--dry-run`, and all are idempotent — safe to re-run at any time without duplicating data.

---

## 13. Project Graph to Native SurrealDB Tables

After ingestion (and optional backfill), run `project` to replay all `graph_events` WAL entries into queryable native tables. This is what populates the tables you can browse in [Surrealist](https://surrealist.app) (the SurrealDB GUI) — remember to set `NS: urban_integrity` and `DB: <analysis id>` there ([Rule 2](#2-an-analysis-id-is-a-database-name-not-a-namespace)).

```bash
cargo run -p urban-integrity-cli -- project
```

Example output (figures vary with the dataset):

```
Done: 23944 nodes upserted, 89601 edges related, 25 nodes skipped, 338 edges skipped
```

The command takes a few seconds for typical datasets. It wipes and fully reprojects these tables:

| Node table | Contents |
|-------|----------|
| `entity` | Companies, organisations and registered societies |
| `officer` | Company directors and officers |
| `location` | Addresses and wards |
| `contract` | Procurement contracts and planning applications |
| `ward` | Ward nodes with deprivation data |
| `political_actor` | Councillors and elected members |
| `psc` | Persons with Significant Control (CH PSC register) |
| `charity` | Charity Commission registered charities |
| `property` | HMLR CCOD/OCOD title records |
| `offshore_entity` | ICIJ Offshore Leaks entities |
| `disqualification` | CDDA disqualification orders (its own node kind since 2026-08-23) |

| Relation table | Contents |
|-------|----------|
| `directs` | officer → entity director edges |
| `located_at` | entity/officer → location edges |
| `awarded_to` | entity (supplier/applicant) → contract edges |
| `issued_by` | entity (buyer/issuing authority) → contract edges |
| `represents` | ward → contract edges (contract or application sited in the ward) |
| `controls` | psc → entity beneficial-ownership edges |
| `trustee_of` | entity → charity edges |
| `owns_property` | entity → property edges |
| `offshore_link` | entity → offshore_entity edges |
| `donates_to` | entity → political_actor donation edges |
| `disqualified_from` | officer → `disqualification` edges (re-pointed 2026-08-23; it previously named `disq:{resource_id}`, an id no `AddNode` ever created) |
| `declares_interest` | political_actor → entity register-of-interests declarations |

> **Three of those tables hold no rows in any live analysis yet.** Measured 2026-08-25 on
> the maintainer's analysis: `disqualification`, `disqualified_from` and `declares_interest` are **0
> projected rows** — and `graph_events` holds **0** `Disqualification`, `DisqualifiedFrom` and
> `DeclaresInterest` events, so the WAL is empty of them too. **`project` alone will not populate
> them.** They were added on 2026-08-23 to fix two structurally unroutable edges, and no analysis
> has been re-enriched since; you need `enrich --source ch_disqualified` (needs the CH **REST** key)
> and `enrich --source council_registers` (needs `[enricher.council_registers] enabled` plus
> non-empty `lpa_codes`) to run first, then `project`. An empty table here is "not yet ingested",
> not "nothing to find".

`project` is idempotent — re-run it freely after more ingestion or backfill to bring the native tables up to date. It folds **`NodeUpdated` alongside `NodeAdded`**, so status corrections (dissolutions, liquidations) written after a node first appeared do reach the projected tables. It fails loud when the skip ratio is catastrophic (at or above 90%) rather than emitting a partial graph.

> **A projected zero is not a graph zero.** The projector builds each row from a `json!` literal, and a field defined in `SCHEMA_DEF` but absent from that literal is dropped with no error. The `SCHEMA_DEF` field-drop guard is a **unit test** covering the `contract` table only — the runtime `project` path does no `SCHEMA_DEF` parsing. Before concluding a field is missing from ingestion, check `graph_events.payload.weight_json` (it is a JSON *string*, so a `payload.weight.X.field` path returns 0 rows rather than erroring). See [docs/PROJECTION.md](PROJECTION.md).

`--dry-run` is accepted as a flag but is not yet implemented — it prints a message and exits without writing.

> **Note:** The `ch_cursor` table will appear empty in Surrealist unless `serve` has been run with a valid `URBAN__INGEST__CH_API_KEY`. This is expected — the cursor is only written when the Companies House stream is active.
>
> **Schema constraints:** The `migrate` subcommand (`cargo run -p urban-integrity-cli -- migrate`) applies IN/OUT direction constraints to all 12 relation tables and re-projects. Run `enrich` before `migrate`. This is a one-time maintenance step, not part of the routine ingest flow.
>
> **Orphan cleanup:** The `purge-orphans` subcommand removes orphan Property nodes and `identity_links` left by a pre-fix CCOD ingest. It is a **dry run by default** (counts only); pass `--apply` to delete. The dry-run count is correct, but **the `--apply` delete path is currently a silent no-op** pending a fix — verify against live data before relying on it. An earlier version of this command misclassified the entire OCOD dataset as orphans.

---

## 14. (Optional) Export Graph to GraphML for Visual Exploration

Export the graph to a Gephi-compatible GraphML file for visual analysis. Two sources are supported
via `--source`, and **the default is `events`** (changed 2026-08-25, commit `722b568` — it was
`native` before that):

- **`events`** (default) — reconstructs the graph directly from the `graph_events` WAL (`NodeAdded`
  + `EdgeAdded`, deduplicated highest-timestamp-per-entity) over the raw HTTP `/sql` endpoint.
  **No `project` step required**, and it is type-agnostic: it reads whatever `"type"` the
  `weight_json` carries, so every node and edge kind in the log is exported, OSINT layer included.
- **`native`** — reads the projected native SurrealDB tables. Requires `project` (Section 13) to
  have been run first, and exits with `No nodes found in native tables` if it has not. It covers
  only 6 node tables (`entity`, `officer`, `location`, `contract`, `ward`, `political_actor`) and 5
  relation tables (`directs`, `located_at`, `awarded_to`, `issued_by`, `represents`) — **the OSINT
  layer is not exported by this path.**

> **Why the default flipped.** `native` goes through the SurrealDB SDK, whose flatbuffers codec
> fails to decode typed `RecordId` fields on large result sets: measured 2026-08-25, a 358k-node
> graph errors outright with `Failed to deserialize flatbuffers payload`. The `events` path
> reads raw HTTP `/sql`, where every id arrives as a plain JSON string, and has no such ceiling.
> `native` is kept for small graphs and for exporting exactly what `project` produced.

> **The `events` export is deterministic.** Nodes and edges are inserted in sorted-key order rather
> than raw hash order, and duplicate `(source, target, edge_type)` triples collapse to the row with
> the greatest `graph_events.timestamp`. Two exports of an unchanged graph are byte-comparable —
> before this, the same 113,250-node graph produced different Parquet column orders on consecutive
> days.

```bash
# Default: straight from the graph_events WAL, no `project` needed
cargo run -p urban-integrity-cli -- export-graphml --output urban-integrity.graphml

# Read the projected native tables instead (run `project` first)
cargo run -p urban-integrity-cli -- export-graphml --source native

# Restrict the export to what is reachable from the configured ward seeds
# (bare flag = every ward_code/lpa_code in config.toml; codes override config)
cargo run -p urban-integrity-cli -- export-graphml --reachable-from-wards
cargo run -p urban-integrity-cli -- export-graphml --reachable-from-wards X05999001,X05999002

# Slice the export by type (both flags repeatable, case-insensitive) and drop
# whatever the filter leaves unconnected
cargo run -p urban-integrity-cli -- export-graphml \
  --node-type Officer --node-type Entity --edge-type Directs --drop-isolated

# Or drop degree-zero nodes from an export you already have
python3 scripts/prune_isolated_nodes.py urban-integrity.graphml -o urban-integrity-connected.graphml
```

**Flags:**

| Flag | Default | Behaviour |
|------|---------|-----------|
| `-o`, `--output <PATH>` | `graph.graphml` | Output file path |
| `--source <native\|events>` | **`events`** | Where the graph is read from (see above) |
| `--node-type <TYPE>` | — | Repeatable, case-insensitive. Keep only nodes of these types; edges whose endpoints were filtered away go with them |
| `--edge-type <TYPE>` | — | Repeatable, case-insensitive. Keep only edges of these types. Nodes are untouched by an edge-only filter, so endpoints survive as isolates unless `--drop-isolated` is passed |
| `--reachable-from-wards [CODE,...]` | off | Restrict to the weakly-connected component reachable from ward seed nodes, applied **before** the type filters. Absent = no reachability filtering. Present with no codes = every `ward_code`/`lpa_code` under `[[ingest.planning_portals]]` in `config.toml`. Present with codes = those codes override config. Seeds that resolve to nothing are a hard error naming every code searched, never a silent empty export |
| `--drop-isolated` | off | Applied **after** type filtering: drops every node left with no edges |

> **An unknown `--node-type` / `--edge-type` is a hard error, not an empty export.** The command
> fails naming both the unrecognised value and every type actually present in the graph, and writes
> nothing. A typo such as `--node-type Offcier` would otherwise produce an empty GraphML file
> indistinguishable from "this analysis has no officers". A *valid* type that legitimately matches
> nothing still returns an empty graph — the two cases are distinguishable precisely because the
> name resolved.

> ### ⚠️ `--source events` ignores `--analysis`
>
> This is current, unfixed behaviour. The `events` branch of `export-graphml`
> (`crates/cli/src/commands/export_graphml.rs`) calls `load_from_events()` with
> `config.surreal.database`, which is sent as the `surreal-db` header verbatim. Only the `native`
> branch calls `connect_db()` → `resolve_analysis_db()` (`crates/cli/src/db.rs`), which is what
> consults the `_catalog` registry. (The workbench's population load uses the same
> `load_from_events()` but passes the *resolved* database, so it is not affected.)
>
> Because `events` is now the **default**, `cargo run -p urban-integrity-cli -- --analysis <id>
> export-graphml` exports whatever `[surreal].database` names — **not `<id>`** — and no log line
> says so. The registry validation is skipped with it: an unknown analysis id does not error, and a
> `retired`/`retiring` analysis is not refused.
>
> Point `[surreal].database` at the analysis you actually mean before exporting, or pass
> `--source native` (which does resolve the selector, at the cost of needing `project` and of the
> large-graph ceiling above). Whatever `[surreal].database` names in the config file in effect is
> what gets exported; any other analysis is not.

Open the resulting file in [Gephi](https://gephi.org) for layout algorithms, community detection, and
path highlighting. The helper script `scripts/export_graphml.sh` wraps `export-graphml --source
events` and writes a timestamped file to the repo root — it reads the `graph_events` WAL directly, so
no `project` step is required. It takes `--debug` to use the debug build, and honours `URBAN_CLI` to
skip the `cargo build` entirely.

---

## Checking the Graph via GraphQL

With the engine running, open `http://localhost:8080/graphiql` (GraphiQL IDE):

```graphql
query {
  alerts(minScore: 0.7) {
    id
    motifType
    score
    detectedAt
    wardCode
  }
}
```

Other endpoints: `/graphql`, `/health`, `/metrics` on `:8080`, and gRPC on `:8081`. See [docs/API.md](API.md).

---

## 15. (Optional) Enable Firecrawl Fallback for Planning Portals

The Scrapling sidecar uses a three-tier fallback cascade when scraping Idox planning portals:

1. **Scrapling** (free, default) — direct HTTP with CSS selectors
2. **Firecrawl stealth GET** — cloud-rendered HTML fetch, used when Scrapling returns 0 results
3. **Firecrawl browser actions** — full browser session (form fill + submit), used when tier 2 also returns 0 results

Tiers 2 and 3 require a Firecrawl API key. Without it, the sidecar logs a warning and continues with Scrapling only — no startup failure.

**To enable Firecrawl fallback:**

Set the API key in the sidecar process environment before starting it:

```bash
FIRECRAWL_API_KEY="fc-your-key-here" ./scripts/sidecar-start.sh
```

The `firecrawl-py` package (`>=1.0.0`) is already listed in `scrapling-sidecar/requirements.txt`. No additional install step is needed.

The same key can be persisted in `config.toml` for the Rust engine's own config surface (`[ingest] firecrawl_api_key`, or `URBAN__INGEST__FIRECRAWL_API_KEY`), but the sidecar reads it exclusively from the `FIRECRAWL_API_KEY` environment variable at startup.

> **Cost note:** Firecrawl browser actions (tier 3) consume significantly more credits than a plain scrape. The sidecar only escalates to tier 3 after tier 2 also returns empty results, so credits are not spent unnecessarily.

---

## 16. Query Entity Profiles

The `entity` subcommand looks up a company by name or Companies House registration number and displays its directors, contracts, related entities, addresses, and any active alerts. The output also includes an LPA presence breakdown showing cross-LPA activity.

```bash
# Look up by company name (table output, default)
cargo run -p urban-integrity-cli -- entity "Apex Build Ltd"

# Look up by Companies House number (exact match)
cargo run -p urban-integrity-cli -- entity --ch-number 12345678

# JSON output for jq pipelines — includes the LPA presence breakdown
cargo run -p urban-integrity-cli -- entity "Apex Build Ltd" --output json | jq '.directors[].name'

# Look up by entity slug (exact ID match — slugs appear in alert output)
cargo run -p urban-integrity-cli -- entity apex-build-ltd

# Find related companies sharing a director
cargo run -p urban-integrity-cli -- entity "Apex Build Ltd" --output json \
  | jq '.relatedEntities[] | select(.sharedConnection.type == "director")'

# Show sub-candidate OSINT links (0.50–0.69 confidence, investigative only)
cargo run -p urban-integrity-cli -- entity "Apex Build Ltd" --deep
```

**Lookup modes:**

| Flag | Behaviour |
|------|-----------|
| Positional argument | Case-insensitive substring name search, or exact slug lookup if the argument matches slug format (tried first) |
| `--ch-number` | Exact match on Companies House registration number (2–10 alphanumeric characters) |
| `--output json` | Machine-readable JSON; defaults to table when stdout is a TTY |
| `--deep` | Adds a "Sub-Candidate OSINT Links (0.50–0.69, investigative only)" section |

The positional name search is a plain case-insensitive substring match against entity names — not fuzzy or phonetic. If more than one entity matches, the command lists candidates and asks you to re-run with `--ch-number` or a slug.

Sub-candidate links surfaced by `--deep` come from the **external yente OpenSanctions service** (which caps name-only matches at 0.65) and the bounded OCOD↔ICIJ matcher; UrbanIntegrity generates no sub-candidate links from in-process fuzzy matching. They cannot be confirmed or rejected via the `links` CLI — they are investigative reference only.

The `entity` command connects directly to SurrealDB — the engine (`serve`) does not need to be running. SurrealDB must be running and reachable at the configured `ws_url` (default `ws://localhost:8000`).

---

## 17. Trace Connections Between Entities

The `graph-path` subcommand finds the shortest path between any two entities in the HIN graph using BFS traversal (up to 6 hops). Use it to trace how a company connects to a councillor, or how two contractors share a common officer.

```bash
# Trace a company to a named individual (table output)
cargo run -p urban-integrity-cli -- graph-path "Apex Build Ltd" "John Smith"

# Use slugs for exact disambiguation when names are ambiguous
cargo run -p urban-integrity-cli -- graph-path GB-COH:12345678 director:john-smith

# JSON output — returns nodes and edges arrays for programmatic processing
cargo run -p urban-integrity-cli -- graph-path "Apex Build Ltd" "John Smith" --output json
```

Example table output:

```
Apex Build Ltd → DIRECTS → John Smith → AWARDS_TO → Example Council
```

Example JSON output:

```json
{
  "nodes": [
    {"id": "GB-COH:12345678", "name": "Apex Build Ltd"},
    {"id": "director:john-smith", "name": "John Smith"},
    {"id": "lpa:X06999001", "name": "Example Council"}
  ],
  "edges": [
    {"type": "DIRECTS"},
    {"type": "AWARDS_TO"}
  ]
}
```

Edge types render in SCREAMING_SNAKE_CASE (e.g. `OWNS_PROPERTY`, `AWARDED_TO`, `DISQUALIFIED_FROM`). If no path exists within 6 hops, the command prints `No path found` and exits cleanly. If a name matches multiple entities, the command lists candidates and asks you to re-run with a slug or CH number.

The `graph-path` command connects directly to SurrealDB — the engine (`serve`) does not need to be running.

---

## 18. OSINT Enrichment and Identity Link Review

The `enrich`, `links` and `flips` subcommands cover OSINT enrichment and analyst-reviewed cross-registry entity linking. All connect directly to SurrealDB — the `serve` engine does not need to be running.

### Run OSINT Enrichment

The `enrich` subcommand runs OSINT enrichment adapters against entities currently in the graph. Adapters query external open-source intelligence sources and write identity link proposals to the `identity_links` table for analyst review. Links are written on **exact-identifier matches only** (registration numbers, exact normalised-name equality) — there is no fuzzy/phonetic matching and no similarity threshold to configure.

`enrich` takes a single `--source` flag (default `all`). Pass `all` to run every enabled adapter, a specific adapter name to run one, or `land-flip` to run the post-adapter land-flip detector.

```bash
# Run all registered enrichment adapters (default)
cargo run -p urban-integrity-cli -- enrich --source all

# Run only the PSC beneficial ownership adapter
# (see Section 11 — the officer backfill MUST have completed first)
cargo run -p urban-integrity-cli -- enrich --source ch_psc

# Run only the ICIJ Offshore Leaks adapter
cargo run -p urban-integrity-cli -- enrich --source icij

# Run the pre-permission land-flip detector (writes to the flips review queue)
cargo run -p urban-integrity-cli -- enrich --source land-flip
```

`scripts/enrich.sh` wraps this command and manages SurrealDB startup via `scripts/surreal-start.sh` (pass `--no-db` to skip if the DB is already running).

Fourteen adapters are registered; each one's `--source` name is shown. Adapters run when enabled and skip silently when their key or compliance gate is absent — see [CONFIGURATION.md](CONFIGURATION.md) for the full key reference:

| `--source` name | Key / gate required | Source | Default state |
|-----------------|--------------------|--------|---------------|
| `ch_psc` | `URBAN__ENRICHER__CH__API_KEY` (REST) | CH PSC beneficial ownership register | Disabled until key set |
| `ch_disqualified` | `URBAN__ENRICHER__CH__API_KEY` (REST) | CH disqualified directors register | Disabled until key set |
| `land_registry_ccod` | `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` | HMLR Corporate Ownership of Land (CCOD) | Disabled until key set |
| `land_registry_ocod` | `URBAN__ENRICHER__OCOD__API_KEY` (falls back to the CCOD key) | HMLR Overseas Companies (OCOD) | Disabled until a HMLR key is present |
| `london_gazette` | None (public API) | London Gazette insolvency notices | Enabled |
| `charity_commission` | None (bulk download, OGL) | Charity Commission register extract | Enabled |
| `icij` | None (bulk download) | ICIJ Offshore Leaks database | Enabled |
| `electoral_commission` | None (public data) | Electoral Commission donations register | Enabled |
| `ppd` | `[enricher.ppd] enabled = true` (explicit `postcode_districts` recommended, see below) | HMLR Price Paid Data (staged to `ppd_transactions` only) | Disabled |
| `mutuals_register` | None (unauthenticated bulk CSV) | FCA Mutuals Public Register — registered societies as `GB-MUT:` Entity nodes | Enabled |
| `planning_identity` | None (internal graph join) | Links `director:{slug}` planning applicants to CH Entity/Officer nodes on exact normalised-name equality | Enabled |
| `opensanctions` | `[enricher.opensanctions] enabled` + `non_commercial_confirmed` (CC-BY-NC gate) | OpenSanctions PEP/sanctions screening via external yente | Disabled |
| `council_registers` | `[enricher.council_registers] enabled` + non-empty `lpa_codes` | Council register-of-interests pages (via the sidecar) | Disabled |
| `opencorporates` | `[enricher.opencorporates] enabled` + `oc_open_data_confirmed` (ODbL gate) + API key | OpenCorporates cross-jurisdiction lookups | Disabled |

Adapters whose credentials or gates are absent return `is_enabled() == false` and are skipped silently by the "run all enabled" path — no startup failure.

> **Keep `[enricher.ppd].postcode_districts` explicit.** Deriving the districts from `Location` nodes pulls in UK-wide registered-office addresses from CCOD/OCOD and passes essentially every Price Paid row in the country.

The enricher spawns a periodic re-auth task that refreshes the SurrealDB sessions (both the engine session and the WAL drain session) every 50 minutes using `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD`, re-selecting the resolved analysis database. If credentials mismatch, a `WARN` is logged and the enricher continues processing.

**Graph mutation in CLI mode.** The CLI `enrich` command wires the graph-mutation channel into a drain task that writes OSINT-sourced nodes and edges (PSC, offshore, property, society nodes) directly to `graph_events`. If `serve` is running concurrently, its in-memory hot graph stays stale until the next `project` run — this is accepted and expected. The land-flip detector and the PPD staging adapter emit **no** graph mutations by design.

Example output (with CH adapters active):

```
Enrichment complete: 14 candidates, 3 auto-linked, 11 pending review.
```

### Review Identity Link Proposals

The `links` subcommand exposes the analyst review workflow for cross-registry entity link proposals stored in the `identity_links` table.

```bash
# List all pending_review identity link proposals (ordered by confidence)
cargo run -p urban-integrity-cli -- links list

# Show full evidence detail for a specific link
cargo run -p urban-integrity-cli -- links show <id>

# Confirm a link — treats the two entity identities as the same real-world entity
cargo run -p urban-integrity-cli -- links confirm <id>

# Reject a link — marks the proposal as reviewed and dismissed
cargo run -p urban-integrity-cli -- links reject <id>
```

**Proposal states:**

| State | Meaning |
|-------|---------|
| `pending_review` | Awaiting analyst review — shown by `links list` |
| `confirmed` | Analyst accepted the link; entities are treated as the same real-world entity |
| `rejected` | Analyst dismissed the link; no graph mutation applied |

`links list` shows only `pending_review` proposals (ordered by confidence, capped at 100 results). Because all proposals come from exact-identifier matches, their confidence reflects the match type — it is not a tunable similarity score. Sub-candidate links (0.50–0.69) are not listed here; inspect them with `entity --deep`.

The `<id>` argument to `show`, `confirm`, and `reject` is the SurrealDB record ID shown in `links list` output (e.g. `identity_links:abc123`).

> **Note:** The `identity_links` table will be empty if no keyed adapter has run. `links list` exits cleanly with an empty table in that case. An empty table straight after a `ch_psc` run usually means the officer layer was empty when the adapter started — see the ordering warning in Section 11.

### Review Land-Flip Candidates

The `flips` subcommand is the review queue for pre-permission land-flip candidates written by `enrich --source land-flip`. It is a subcommand group with `list`, `show`, `confirm`, and `dismiss`. Confirming or dismissing is a **review-queue action only** — it never writes to `graph_events` or the hot graph.

```bash
# List pending land-flip candidates (newest first, capped at 100)
cargo run -p urban-integrity-cli -- flips list

# Show all three gate-evidence sections for one candidate
cargo run -p urban-integrity-cli -- flips show <id>

# Confirm a candidate as a genuine land-flip
cargo run -p urban-integrity-cli -- flips confirm <id> --note "analyst note"

# Dismiss a candidate
cargo run -p urban-integrity-cli -- flips dismiss <id> --note "false positive"
```

Candidates are capped at **MEDIUM severity** by a compile-time-enforced invariant — there is no HIGH variant, and no buyer is ever named in the hot graph. A run prints where every application died (gate-1 no PPD match, gate-2 no anchor date or outside window, gate-3 no corroboration), because a bare "0 candidates" reads identically for four different problems with four different fixes.

---

## 19. Pre-Signature Counterparty Audit

Before signing a contract with a counterparty, use `verify` to run a risk audit against the graph and the OSINT enrichment layer.

```bash
# Audit a counterparty by CH number
cargo run -p urban-integrity-cli -- verify --company-number 12345678

# Add a one-line Ollama/Gemma summary (graceful WARN if Ollama is unavailable)
cargo run -p urban-integrity-cli -- verify --company-number 12345678 --summarize

# Use exit codes in CI pipelines
cargo run -p urban-integrity-cli -- verify --company-number 12345678 && echo "Safe to proceed" || echo "Risk detected"
```

**Exit codes:**
- `0` — low or medium risk (no critical findings), or entity not found in graph
- `1` — high or critical risk (disqualified director evasion detected, or confirmed disqualification)

**What it checks:**
- Whether any director has been disqualified by the Insolvency Service (requires `URBAN__ENRICHER__CH__API_KEY`)
- Whether a disqualified director is actively evading their ban via the same company (Evasion Detector)
- Beneficial ownership via the PSC register
- Existing motif alerts from the core detection engine

The `--summarize` flag calls Ollama at `http://localhost:11434/v1` (expects a Gemma-family model) and degrades gracefully with a `WARN` if Ollama is unavailable.

The command connects directly to SurrealDB — the `serve` engine does not need to be running.

---

## 20. Provision and Manage Analyses

Each investigation ("analysis") lives in its own SurrealDB **database**, tracked in the `analyses` table of a `_catalog` registry database under the `urban_integrity` **namespace** ([Rule 2](#2-an-analysis-id-is-a-database-name-not-a-namespace)). The shipped `config.toml` points at `an_example_00000000` — a fictional id that names an empty database until you create an analysis of your own and select it (`--analysis`, or `[surreal] database` in your `config.local.toml`). Database names are content-addressable: a slug of the label plus a hash of the ward set.

### Select an Analysis

Every subcommand accepts a global `--analysis` flag; it is bridged internally to the `URBAN__ANALYSIS` environment variable, so both forms are equivalent:

```bash
# Operate on a named analysis (the registry db_name)
cargo run -p urban-integrity-cli -- --analysis an_planted_fixture_xxxxxxxx alerts list

# Equivalent via environment variable
URBAN__ANALYSIS=an_planted_fixture_xxxxxxxx cargo run -p urban-integrity-cli -- alerts list
```

Omitting the flag (or passing `main`) uses `config.toml [surreal] database` verbatim, with no registry lookup. Any other value is resolved through the `_catalog` registry to the analysis's database name; unknown ids and `retired`/`retiring` analyses error out.

### Create an Analysis

`analysis create` provisions a new analysis database from a ward list — optionally cloning an existing baseline database with `--from`:

```bash
# wards.txt: one ONS GSS ward code per line.
cargo run -p urban-integrity-cli -- analysis create \
  --label "Black Country 2026" \
  --wards-file wards.txt \
  --boundary-vintage 2024

# Clone an existing baseline database instead of provisioning empty
cargo run -p urban-integrity-cli -- analysis create \
  --label "Black Country 2026" --wards-file wards.txt --from uk_national
```

Creation is idempotent-by-refusal: re-running with the same label and ward set fails with `analysis already exists`, because the database name is derived from that content. A missing `--from` baseline errors **before** any provisioning. `--from` clones the baseline whole-DB via `surreal export | surreal import`, so the `surreal` binary must be available — located via `SURREAL_BIN`, then `~/.surrealdb/surreal`, then `PATH`. (`export`/`import` are not intercepted by `surreal-guard.sh`; only `start` is.)

A new analysis's wards must all belong to one nation, and its deprivation index must match that nation — the startup preflight refuses otherwise. See Step 4.

**Fixture analyses.** `analysis create --fixture` marks the new registry row as a fixture target. It is the only kind of analysis `fixture seed` will write into — see [Section 21](#seed-the-planted-toy-analysis). `scripts/toy-analysis.sh` creates one for you.

### List Analyses

```bash
cargo run -p urban-integrity-cli -- analysis list
```

Shows every registry row with its `db_name`, label, ward count, status, and creation time. The `db_name` column is what you pass to `--analysis` and `analysis remove`.

### Remove an Analysis

`analysis remove` is a **dry run by default** — it reports the label, ward count, event count, and Phoenix-window status without mutating anything:

```bash
# Dry-run (default): report what would be removed
cargo run -p urban-integrity-cli -- analysis remove an_foo_deadbeef

# Actually drop the analysis database, delete its checkpoint dir, and retire the registry row
cargo run -p urban-integrity-cli -- analysis remove an_foo_deadbeef --apply

# An analysis still inside the Phoenix detection window holds live evidence —
# removal is refused unless you explicitly decommission it
cargo run -p urban-integrity-cli -- analysis remove an_foo_deadbeef --apply --decommission
```

The Phoenix-window guard reads `phoenix_window_days` from `config.toml [detector]` (default 180): an analysis younger than the window is refused by `--apply` unless `--decommission` is also passed. Reserved databases can never be removed, even with `--apply --decommission`. Removal is crash-safe: the row is marked `retiring` before the drop and only stamped `retired` afterwards, so an interrupted remove is never mistaken for a healthy analysis.

---

## 21. Analyst Workbench (Browser UI)

The workbench is a browser UI over a single analysis database: a Home launcher, the interactive
probe report, alert triage, entity search and profiles, a graph canvas with graph algorithms,
saved investigations, and an in-app Help guide with a compiled glossary. It is **replay-only** —
at startup it replays the analysis's `graph_events` into an in-memory engine and serves that
snapshot until you stop it. It never ingests, streams, enriches or re-projects. The writes it does
make are analyst actions: running the probe battery (the result is stored as the analysis's latest
report), withdrawing an alert, and creating or editing saved investigations.

### Build the frontend

The Leptos/wasm frontend (`crates/workbench-web`) is a separate, explicit build step — never a
`build.rs` side effect — so `cargo build --workspace` keeps working on a machine with no Trunk, no
Node and no wasm target:

```bash
cargo install --locked trunk --version 0.21.14   # once
scripts/build-workbench.sh
```

The script compiles `crates/workbench-web` to `wasm32-unknown-unknown` with `trunk build --release`
and writes the assets to `crates/workbench/dist/` (not `crates/workbench-web/dist/` —
`crates/workbench-web/Trunk.toml` points there because the native `workbench` crate embeds that
folder with `rust-embed`). `dist/` is gitignored.

> **The `trunk` on your PATH may be the wrong program.** `pg-trunk`, a PostgreSQL extension
> manager, installs a binary with the same name. `build-workbench.sh` checks `trunk --version` and
> refuses to run if it reports `pg-trunk`, or anything other than `trunk X.Y.Z`. If you need both,
> install the bundler into its own root and put it first on `PATH` for the build:
>
> ```bash
> cargo install --locked trunk --version 0.21.14 --root ~/.local/trunk-bundler
> PATH="$HOME/.local/trunk-bundler/bin:$PATH" scripts/build-workbench.sh
> ```
>
> Always pass `--locked --version 0.21.14`; a bare `cargo install trunk` can overwrite whichever
> `trunk` binary is already in `~/.cargo/bin`.

> **Rebuild the binary after every frontend build.** The `workbench` crate enables rust-embed's
> `debug-embed` feature, so `dist/` is baked into the binary at compile time in debug builds as
> well as release. A workbench started before (or compiled before) the latest
> `build-workbench.sh` keeps serving the old frontend. Force the re-embed and restart:
>
> ```bash
> touch crates/workbench/src/lib.rs
> cargo build -p urban-integrity-cli
> ```

The Cosmograph graph renderer is loaded from a committed, pinned bundle
(`crates/workbench-web/js/cosmograph-shim.js`), so no Node or npm step is part of the build.

### Seed the planted toy analysis

For a first look — and for checking the UI without touching a live corpus — use the planted
fixture: a small synthetic graph defined in `crates/test-support/src/planted.rs` under a
placeholder ward code, built so that the probe battery and the graph algorithms have something
to find.

```bash
scripts/surreal-start.sh      # from the repo root (Rule 1)
scripts/toy-analysis.sh
```

`toy-analysis.sh` is idempotent. It:

1. Reads the fixture's ward code from `planted.rs` and fails fast if SurrealDB is not answering
   on `:8000`.
2. Looks for an analysis labelled `Planted fixture` in `analysis list`, and creates one with
   `analysis create --label "Planted fixture" --wards-file <tmp> --fixture` if it is missing.
3. Runs `fixture seed --planted --reseed` against it, then `project`.
4. Runs `probe --output data/toy-evidence-report.md`.
5. Prints the `workbench` command to run next, with the analysis's database name filled in.

Re-run it whenever the fixture or the probes change — a toy database seeded by an older build
reads as mostly `DATA_GAP` and looks like a fixture that is not comprehensive. The same steps by
hand:

```bash
echo X05999001 > /tmp/planted-wards.txt       # the WARD_CODE constant in planted.rs
cargo run -p urban-integrity-cli -- analysis create \
  --label "Planted fixture" --wards-file /tmp/planted-wards.txt --fixture
cargo run -p urban-integrity-cli -- analysis list  # note the db_name it was given
cargo run -p urban-integrity-cli -- --analysis <db_name> fixture seed --planted
cargo run -p urban-integrity-cli -- --analysis <db_name> project
```

`fixture seed` is guarded so it cannot land in real data:

- It needs an explicit `--analysis`; it never targets the default database.
- It refuses any analysis whose registry row was not created with `analysis create --fixture`.
- It refuses a target whose `graph_events` is not empty unless `--reseed` is passed. `--reseed`
  wipes `graph_events`, `identity_links`, `enrichment_runs`, `land_flip_runs` and
  `land_flip_gate1_survivors` in that analysis before rewriting them.
- `--planted` is currently the only fixture; the command exits with `nothing to seed` without it.

On success it prints the node and edge counts written and the manifest's reachable-node count from
the fixture ward.

### Start the workbench

```bash
cargo run -p urban-integrity-cli -- --analysis <db_name> workbench
# Workbench: http://127.0.0.1:8090
```

Omitting `--analysis` serves `config.toml [surreal] database`. The subcommand's own flags:

| Flag | Behaviour |
|------|-----------|
| `--bind <addr>` | Override the bind address for this run. Precedence, lowest to highest: `config.toml [workbench] bind_addr` (`127.0.0.1:8090`), `URBAN__WORKBENCH__BIND_ADDR`, then this flag |
| `--open` | Open the URL in your default browser once the listener is bound. Off by default |

The listener is bound **before** anything else, so a port that is already taken fails in
milliseconds with an error naming the address — the workbench never picks another port. The URL
is printed as soon as the socket exists; replaying the analysis follows, which takes minutes on a
live corpus. A browser tab opened meanwhile waits rather than being refused.

Stop it with Ctrl-C. If port 8090 is still held after a workbench you thought you had stopped
(killing the `cargo run` parent from elsewhere can orphan the child that owns the socket), find the
holder with `ss -ltnp | grep 8090` and stop it.

> **The CLI `probe` does not feed the workbench's report.** `/analysis` shows the last battery run
> *from the workbench* — its Run button stores the result in the analysis database and it is
> reloaded on the next start. A report written by `urban-integrity-cli probe` (including the one
> `toy-analysis.sh` writes to `data/toy-evidence-report.md`) stays a Markdown file, so a fresh
> workbench says no probe has run yet until you press Run.

### Pages

The header, present on every page, carries a link to saved investigations, the **analysis
picker**, and a light/dark theme toggle. One workbench process is bound to one analysis: choosing
a different entry in the picker shows the exact command to restart against it rather than
switching live. Every page other than Home has a breadcrumb back to Home; there is no sidebar.

| Route | What you see |
|-------|--------------|
| `/` | Home — four launcher cards (Analysis, Graph, Ingestors, Help), each summarising its destination's current state |
| `/analysis` | The probe report: a Run button, verdict groups, an accordion down to each probe's evidence, `#<probe-name>` deep links, and a Markdown download of the report |
| `/finding/:probe` | One probe's finding in detail |
| `/alerts`, `/alert/:id` | Motif alert triage and alert detail, including withdrawing an alert (with a reason, as `alerts withdraw` requires) |
| `/graph` | Population metadata — node/edge counts and the ward-reachable population with its generation timestamp, or an explicit "population unavailable" state — above an interactive Cosmograph canvas of that population, with a node side card, canvas filters and the graph algorithms (shortest paths, all simple paths, community detection, k-core, betweenness, ward subgraph) |
| `/entity`, `/entity/:id` | Entity search and entity profiles, with a neighbourhood view |
| `/investigations`, `/investigations/:id` | Saved investigations: pinned entities and subgraphs with notes |
| `/help` | The in-app guide: the report's vocabulary, the verdict badge legend, the graph algorithms, and a CLI cheat-sheet with the current `--analysis <db>` already substituted into each command |
| `/help/algorithms`, `/help/algorithms/:slug` | One page per graph algorithm |
| `/help/glossary` | A searchable table of every abbreviation `docs/abbreviations.md` defines — the same source the inline `<abbr>` tooltips read |

The population the graph page shows is the same ward-reachable population `export-graphml
--reachable-from-wards` computes, seeded from the analysis's own registered ward codes (or the
`config.toml` wards when running without a registry entry).

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `cargo build` fails in `urban-integrity-api` with a `protoc`/`tonic-build` error | Install the Protocol Buffers compiler: `apt-get install protobuf-compiler` (macOS: `brew install protobuf`), then rebuild |
| `Connection refused` / `error sending request for url http://localhost:8000/health` | SurrealDB is not running — start it with `./scripts/surreal-start.sh` (never a bare `surreal start`) |
| **Queries hang while `/health` still returns ok** — and `surreal-start.sh` reports "already running, nothing to do" | The instance is wedged and the launcher will not replace it. `systemctl --user stop urban-surreal` **then** `scripts/surreal-start.sh`. Crash-safe; RocksDB replays its WAL |
| **A hand-written HTTP/Surrealist query returns zero rows with no error** | You put the analysis id in the `NS` slot. Namespace is always `urban_integrity`; the analysis id (`an_…`) is the **DB**. SurrealDB silently creates the empty namespace you asked for |
| `serve` logs `IMD index load failed`; every alert shows `m_env = 1.0` and a null `imd_rank` | `data/*` is gitignored, so no deprivation index ships. Build one: `python3 scripts/build_deprivation_ward_index.py --nation <nation> --lad <codes>` (Step 4) |
| `build_deprivation_ward_index.py` exits with "`--nation <n>` needs the local authorities to cover" | `--nation` alone is not enough for wales/england/scotland — pass `--lad` (or `--all`, wales and northern-ireland only) |
| Startup refused with a jurisdiction/nation error | The configured wards span two nations, or `imd_csv_path` points at a different nation's index. One nation per analysis, one nation per index file |
| `Background launcher … NO memory cap` warning from `surreal-start.sh` | No systemd user session, so SurrealDB runs uncapped ([step A4](#a4-start-surrealdb)). Fine for the toy analysis; for a real ingest use a host with systemd (on WSL2, enable it in `/etc/wsl.conf`) |
| `Anonymous access not allowed` | `config.toml [surreal]` must have `username = "root"` and `password = "root"` |
| `.env` settings appear to be ignored | Check every name has a **double** underscore after `URBAN` (`URBAN__SURREAL__USERNAME`). The loader's prefix is `URBAN__`; single-underscore names are silently skipped. Also run from the repo root, where `.env` lives |
| `The table 'graph_events' does not exist` | Schema not applied — run any CLI command that connects (`cargo run -p urban-integrity-cli -- status` is the lightest); `connect_to_db()` applies `SCHEMA_DEF` idempotently on connect |
| `status` reports 0 nodes / 0 edges on a populated analysis | Check the `Projection` line. `never run` means the event log has never been projected into node and relation tables — run `project`. If it says `complete` and the counts are genuinely 0, the log itself is empty (`Graph events: 0`), so the problem is upstream in ingestion. Before 2026-09-03 `status` was a stub printing hardcoded zeros; if you see `last_snapshot_event` or `uptime_secs` in its output you are on an old build |
| Every command fails with `unknown field ..., expected one of analysis, surreal, ...` | An unknown `URBAN__*` section is exported in your shell. `AppConfig` is `deny_unknown_fields`; there is no `[db]` section — the endpoint is `URBAN__SURREAL__WS_URL`. Unset the stray variable |
| `URBAN__INGEST__CH_API_KEY not set or empty — CH streaming disabled` (WARN from `serve`) | Set `URBAN__INGEST__CH_API_KEY` (the **streaming** key) for `serve`. The `backfill` subcommands need a different credential — `URBAN__ENRICHER__CH__API_KEY` (the **REST** key) |
| CH REST calls return `401` from a Python script or the enricher | You are using the streaming key on a REST endpoint. Set `URBAN__ENRICHER__CH__API_KEY` (the **REST** key) — the two are not interchangeable |
| `CH API 429 rate limit` during `backfill ch` | CH allows 600 requests / 5 minutes. Keep `--rate-limit 2` (the default); higher rates 429 within a minute |
| `cargo build` fails with dependency errors | Confirm the pinned toolchain with `rustup show` (it should report `1.96.0`); run `rustup update` to refresh it |
| No alerts after ingestion | The graph may not have enough data — wait for a snapshot cycle (`snapshot_every_n_events = 1000` by default). Phoenix in particular produces nothing on a fresh graph by data, not by fault. Note that `alerts list --motif phoenix` normalises the spelling but `report --motif phoenix` does not — use `PhoenixLoop` for `report` |
| Can't reach GraphQL | Check the engine is running; HTTP is on `:8080`, not `:3000` |
| Can't reach gRPC | gRPC is on `:8081` (HTTP port + 1) |
| **A `URBAN_DB_TESTS=1` run finished in seconds and everything passed** | Distrust it. The likeliest cause is a SurrealDB launched from a git worktree, serving a fresh EMPTY per-worktree store: `/health` is 200 and the pass count is identical, only the duration dissents. Check `systemctl --user show urban-surreal -p WorkingDirectory` is the repo root and that `INFO FOR ROOT` lists `urban_integrity` ([Rule 1](#1-start-surrealdb-only-via-scriptssurreal-startsh--never-surreal-start)) |
| `URBAN_DB_TESTS=1 cargo test` skips all DB tests | SurrealDB must be listening on `localhost:8000` before running tests |
| A green `cargo test --workspace` without `URBAN_DB_TESTS=1` | Expected, and it proves nothing about the DB-gated tests — the gate is skip-by-return, so both runs report the same passing count. Use `scripts/check.sh --db` |
| **`urban-integrity-cli probe` reported `SKIPPED` for most of the battery** | Working as intended — dispatch is verdict-gated by default. A Tier-2 probe runs only if at least one Tier-1 probe returned `HIT` or `WEAK`, so an analysis whose Tier-1 probes all come back `CLEAR`/`DATA_GAP`/`BLOCKED` skips all 15 Tier-2 probes at once. Run `probe --full-battery` for the ungated 22-probe comparison |
| `probe` reports `SKIPPED — upstream 'identity-integrity' returned …` for `address-cluster-exact` | You are on an old build: that dependency edge was removed on 2026-09-03 and the current battery declares none. Rebuild, or run `probe --full-battery` |
| A `probe` run and a `probe-oracle` run disagree about which probes ran at all | Expected: `probe` gates by default, `probe-oracle` always runs the battery ungated. Compare `probe --full-battery` against `probe-oracle`, not a bare `probe` |
| `probe-oracle` aborts saying the analysis was repointed while the battery ran | An `analysis` retire or repoint landed inside the run window. Nothing is wrong with the graph — re-run `probe-oracle` once the registry is settled |
| `urban-integrity-cli probe` reports `DATA_GAP` for `precomputed-alerts` | No `detector_runs:current` marker exists — the analysis has not run `serve` since that marker shipped. This is the battery refusing to call an unrun detector "clear"; run `serve` to write it |
| `probe-oracle` shows a probe disagreeing with the oracle | Real signal — investigate the probe's counting shape before anything else, and never "fix" it by adjusting either side to match. The allowed delta is 0 for every probe, so any difference shows. Known benign case: `address-cluster-exact` (the probe needs both edge endpoints to parse; the oracle counts distinct destinations regardless) |
| `probe-oracle` shows `not measured this run` | That probe returned `DATA_GAP` or `BLOCKED`, so it declared no population. Nothing is compared — this is deliberate, so an unmeasured probe cannot look like a disagreement against 0 |
| A `probe-oracle --output` file is full of `ESC[` escape sequences | You are on an old build — the artifact is now rendered from de-styled tables. Rebuild, or strip it with `sed -r 's/\x1b\[[0-9;]*m//g'` |
| `analyst_probe.py` returns only `DATA_GAP` verdicts | Run the full pipeline steps 1–7 (Section 11) first to populate the identity layer |
| A Python script exits with "no analysis database selected" | There is no default target by design — pass `--analysis <db_name>` or set `URBAN__ANALYSIS`. Use `analysis list` to see valid names |
| `enrich --source ch_psc` reports success but writes **0 identity links** | The officer layer was empty when the adapter pre-loaded its officer-id set. Run `backfill officers` first, then repair with `backfill psc-links` (no CH API calls) |
| `ch_cursor` table empty in Surrealist | Expected — the cursor is only written when `serve` runs with a valid CH streaming key |
| Native tables empty in Surrealist after ingestion | Run `cargo run -p urban-integrity-cli -- project` to replay WAL events into native tables — and check `NS`/`DB` are set correctly |
| `disqualification`, `disqualified_from` or `declares_interest` is empty after `project` | Expected on every current analysis — `graph_events` holds 0 events of those kinds, so `project` has nothing to project. Run `enrich --source ch_disqualified` and `enrich --source council_registers` first, then re-`project` |
| A projected column reads zero while the data looks present | The projector drops schema-defined fields absent from its `json!` literal, silently. Check `graph_events.payload.weight_json` (a JSON *string*) before blaming ingestion. See [docs/PROJECTION.md](PROJECTION.md) |
| `export-graphml --source native` fails with "No nodes found in native tables" | The projected tables are empty — run `project` first, or drop the flag (`--source events` is the default and reads the WAL directly, no projection needed) |
| `export-graphml --source native` fails with `Failed to deserialize flatbuffers payload` | The SurrealDB SDK's flatbuffers codec cannot decode typed `RecordId` fields on large result sets — a 358k-node graph hits it. Drop `--source native` and use the default `events` path, which reads raw HTTP `/sql` and has no such ceiling |
| `export-graphml --node-type X` errors listing the types present | Working as intended — an unrecognised type is a hard error, never a silent empty export. Copy a name from the list it printed (matching is case-insensitive) |
| **`--analysis <id> export-graphml` exported the wrong analysis** | Known, unfixed: `--source events` (the **default**) reads `[surreal].database` directly and ignores `--analysis` entirely — including the registry checks, so an unknown or retired id does not error. Point `[surreal].database` at the analysis you mean, or pass `--source native`. See Section 14 |
| `enrich --source all` produces 0 candidates | `URBAN__ENRICHER__CH__API_KEY` is not set — the CH enricher adapters skip silently without it |
| `enrich --source all` skips CCOD/OCOD | `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` is not set — set it and re-run |
| Fewer identity links than expected | Linking is exact-identifier only (no fuzzy matching as of 2026-06-13) — verify the source records carry matching CH numbers / normalised names. There are no thresholds to tune |
| Planning scrape returns nothing | Check the sidecar with `curl localhost:8765/health`; start it with `./scripts/sidecar-start.sh`. Note the WFS, PlanAPI and Northgate strategies are stubs that return empty results |
| A ward scrape times out | A ward re-list is one request covering the whole ward. Raise `[ingest] scrapling_timeout_secs` (default 300; a 4,986-application ward measured 157.6 s) |
| `urban-surreal` service won't start / exits during startup (OOM) | Check `data/surreal.log` and `journalctl --user -u urban-surreal`. Raise the cap with `URBAN_SURREAL_MEM_MAX=16G ./scripts/surreal-start.sh` if your host has headroom |
| `surreal binary not found at ~/.surrealdb/surreal` | Install SurrealDB v3 to `~/.surrealdb/surreal`, or point the launcher at it with `SURREAL_BIN=/path/to/surreal ./scripts/surreal-start.sh` |
| `[surreal-guard] Refusing a bare 'surreal start'` | Working as intended — use `./scripts/surreal-start.sh`. The guard prevents reintroducing the uncapped surrealkv OOM by reusing old shell history. Only `start` is intercepted; `sql`/`import`/`export` pass through |
| `enricher DB re-signin failed` | `URBAN__SURREAL__USERNAME` / `URBAN__SURREAL__PASSWORD` do not match the running SurrealDB instance — verify against the `--user root --pass root` credentials the launcher uses |
| `Transaction conflict: Resource busy` during a long enrich run | A concurrent full re-ingest is thrashing SurrealDB's background tasks. Kill only the `enrich` client; if queries still hang while `/health` is up, `systemctl --user stop urban-surreal` then `scripts/surreal-start.sh` |
| `verify` exits 1 unexpectedly | Check `--company-number` format (2–10 ASCII alphanumeric characters; English 8-digit, Scottish SC, NI, LLP OC and foreign FC prefixed formats are all valid). A known evasion alert for this company may be present — review with `alerts list --motif disqualified_director_evasion` |
| `unknown analysis '<id>'` | The `--analysis` value must be a registry `db_name` — run `analysis list` to see known analyses. `main` (or omitting the flag) uses the `config.toml [surreal] database` directly. **On a fresh clone the `_catalog` registry is empty**, so *every* explicit `--analysis` value fails until you run `analysis create`; omit the flag instead |
| `report.sh` dies with `SurrealDB not found at ~/.surrealdb/surreal` even though `SURREAL_BIN` is set | `scripts/report.sh` hardcodes `~/.surrealdb/surreal`; only `scripts/surreal-start.sh` honours `SURREAL_BIN`. Symlink the binary into `~/.surrealdb/`, or start SurrealDB yourself and call the `report` subcommand directly |
| `build_deprivation_ward_index.py --nation northern-ireland` exits asking for LibreOffice | NISRA publishes NIMDM only as legacy `.xls`. Install LibreOffice, or convert by hand with the `soffice --headless --convert-to xlsx` command the error prints |
| `failed to load data/party_authority_control.json — skipping detector` | That file *is* tracked and should arrive with a clone — check you are running from the repo root, since the enricher reads it by relative path |
| `analysis remove` refuses with a Phoenix-window error | The analysis is younger than the Phoenix detection window (`phoenix_window_days`, default 180) and still holds live evidence — pass `--apply --decommission` to retire it deliberately |
| `anomalies show <id>` reports "no such anomaly" | Pass the id exactly as `anomalies list` prints it — anomaly keys legitimately contain colons and must not be truncated at the first one |
| `build-workbench.sh` says the `trunk` on PATH is pg-trunk | A PostgreSQL extension manager owns the name. Install the bundler with `cargo install --locked trunk --version 0.21.14 --root ~/.local/trunk-bundler` and prepend `~/.local/trunk-bundler/bin` to `PATH` for the build (Section 21) |
| `build-workbench.sh` says `wasm32-unknown-unknown` is not installed | Run any `cargo` command in the repo root so rustup applies `rust-toolchain.toml` (which lists the target), or `rustup target add wasm32-unknown-unknown` |
| The workbench page says "Workbench assets not built" | `crates/workbench/dist/` did not exist when the binary was compiled. Run `scripts/build-workbench.sh`, then `touch crates/workbench/src/lib.rs && cargo build -p urban-integrity-cli` and restart |
| The workbench still shows the old UI after a frontend rebuild | The assets are embedded at compile time (`debug-embed`). Rebuild the binary as above and restart; also check nothing stale still holds the port (`ss -ltnp \| grep 8090`) |
| `workbench` fails immediately naming the bind address | The port is taken — usually an earlier workbench that is still running. Stop it, or pass `--bind 127.0.0.1:<other port>` |
| `/analysis` says no probe has run, although `probe` was just run from the CLI | Expected — the CLI `probe` writes Markdown only. Press Run on the page; that result is stored and reloaded on the next start |
| `fixture seed` refuses: `was not created with analysis create --fixture` | Working as intended — fixtures only go into fixture analyses. Create one with `analysis create --fixture`, or just run `scripts/toy-analysis.sh` |
| `fixture seed` refuses: `graph_events already holds N rows` | Pass `--reseed` to wipe and rewrite the fixture analysis |
| The toy analysis reports mostly `DATA_GAP` | The toy database was seeded by an older build. Re-run `scripts/toy-analysis.sh` to reseed, re-project and re-probe it |
| Test namespaces accumulating in the local store | `Drop` cannot fire when a test binary is SIGKILLed. Sweep with `scripts/surreal-drop-test-ns.sh --apply` (dry-run by default; only ever touches names starting with `test`) |

---

## Next Steps

- [docs/CONFIGURATION.md](CONFIGURATION.md) — every config key and environment variable
- [docs/PROJECTION.md](PROJECTION.md) — how `graph_events` and the projected tables relate, and what a projected zero does not mean
- [docs/ARCHITECTURE.md](ARCHITECTURE.md) — system design, crate map, and data flow
- [docs/DEVELOPMENT.md](DEVELOPMENT.md) — contributor workflow and crate structure
- [docs/TESTING.md](TESTING.md) — test inventory, DB-gate semantics, and namespace hygiene
- [docs/API.md](API.md) — GraphQL, gRPC, and HTTP endpoint reference
- [README.md](../README.md) — project overview, CLI reference, and known gaps
