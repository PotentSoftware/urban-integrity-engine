<!-- generated-by: gsd-doc-writer -->
# Probe Battery Reference

> **⚠️ Work in progress — this software makes mistakes.** Its alerts, scores and reports are
> leads for qualified review, never evidence of wrongdoing. Do not accuse any person or
> organisation on the basis of its output unless a qualified fraud investigator is involved and
> you have taken legal advice first. See [DISCLAIMER.md](../DISCLAIMER.md).

The `urban-integrity-probe` crate (`crates/probe`) holds the **probe battery**. That is a fixed,
ordered set of 22 forensic checks that run against one analysis database. Each probe answers a
single question about the graph and returns one typed `Finding`. The battery's output is a
Markdown evidence report plus a structured view that the workbench renders.

The crate is a standalone workspace member so that the CLI (`crates/cli`) and the workbench
(`crates/workbench`) can both run the battery in-process through the same entry point,
`urban_integrity_probe::run::execute`. The dependency direction is `probe -> core / schema /
enricher`. Nothing in the crate depends on `cli`.

## Overview

| Concern | Where it lives |
|---|---|
| `Probe` trait, `Strategy`, `MotifKind`, ungated `run_probes` | `crates/probe/src/lib.rs` |
| Battery registration (`build_battery`) and entry point (`execute`) | `crates/probe/src/run.rs` |
| Dependency graph, validation, gated dispatch (`run_gated`) | `crates/probe/src/graph.rs` |
| Population provenance: `CHECKED_PROBES` / `EXCLUDED_PROBES` | `crates/probe/src/population_source.rs` |
| Source-attributed evidence counts (`Attributed`) | `crates/probe/src/attributed.rs` |
| Shared disqualification-chain read and cause-to-verdict mapping | `crates/probe/src/disqualification_chain.rs` |
| Markdown report writer | `crates/probe/src/report/` |
| `Verdict`, `Coverage`, `Finding`, `RecordRef`, `Severity`, `Cause` | `crates/schema/src/types/verdict.rs` |
| CLI `probe` / `probe-oracle` subcommands | `crates/cli/src/commands/probe.rs`, `probe_oracle.rs` |
| Workbench run/report endpoints | `crates/workbench/src/analysis.rs`, `report_persistence.rs` |

## Probe catalogue

This table comes from each probe's own `name()`, `tier()`, `evidence_scope()` and
`motif_kind()` implementations, listed in the registration order used by `build_battery`
(`crates/probe/src/run.rs`). The **Id** is the literal string a probe reports in
`Finding.probe`. In a few cases it differs from the module name.

### Tier-1: graph probes

| # | Id | Module | Question the probe answers | Main inputs | Evidence scope | Motif |
|---|---|---|---|---|---|---|
| 1 | `precomputed-alerts` | `precomputed_alerts.rs` | Has the live motif detector already computed alerts, and are there any? | `MotifDetected` events (`motif_type` is parsed out of `payload.result_json`) | NamesNodes | Chips |
| 2 | `identity-integrity` | `identity_integrity.rs` | Is applicant-provenance coverage adequate across planning Contracts, and can the `_catalog` coverage baseline be reached? | Latest-per-entity planning Contracts, plus `_catalog` `analyses.coverage_baseline_json` over a second connection | AggregateOnly | Absence |
| 3 | `detector-readiness` | `detector_readiness.rs` | Does the graph contain every term each motif detector (Phoenix loop, Nepotism bridge, Address cluster) needs, and did the Tier-B detector run? | Full paged scans of events and entities by type, dissolution events, and the detector run marker | AggregateOnly | Absence |
| 4 | `address-cluster-exact` | `shared_exact_address.rs` | Do two or more distinct planning applicants share exactly the same registered address, excluding buyers and formation agents? | Location nodes with at least one applicant, plus `Strategy.identity_blocked` | NamesNodes | Star |
| 5 | `geographic-concentration` | `geographic_concentration.rs` | Are planning applications concentrated on particular streets or postcode sectors, and is the Location population non-zero? | Latest-per-entity Location nodes | AggregateOnly | Absence |
| 6 | `ward-concentration` | `ward_concentration.rs` | Are planning applications concentrated in a small number of wards? | `Represents` edges (ward to contract), deduplicated in Rust | NamesNodes | RankedBars |
| 7 | `temporal-burst` | `temporal_burst.rs` | Does Contract ingestion spike on a single day, measured against the whole Contract population? | Contract nodes and their timestamps | NamesNodes | Chips |

### Tier-2: OSINT and anchor probes

| # | Id | Module | Question the probe answers | Main inputs | Evidence scope | Motif |
|---|---|---|---|---|---|---|
| 8 | `identity-link-registry` | `identity_link_registry.rs` | Does `identity_links` hold any confirmed or pending resolutions? Later Tier-2 probes read this result through `Strategy`. | The whole `identity_links` table plus the `enrichment_runs` markers | NamesNodes | Chips |
| 9 | `osint-evasion-alerts` | `evasion_alerts.rs` | Is there a `disqualified_director_evasion` alert for a director under a live disqualification order? | `MotifDetected` rows and the shared disqualification chain | AggregateOnly | Absence |
| 10 | `osint-disqualified-directors` | `disqualified_active_in_procurement.rs` | Is a disqualified director linked to an entity that holds a contract award? | `ch_disqualified` links, `AwardedTo` edges (paged intersection) and the disqualification chain | NamesNodes | Chips |
| 11 | `psc-beneficial-ownership` | `psc_beneficial_ownership.rs` | Do PSC records exist for procurement entities, and do any show control by a disqualified or sanctioned individual? | `ch_psc` identity links and the `ch_psc` run marker | NamesNodes | Chips |
| 12 | `gazette-insolvency-signals` | `gazette_insolvency_signals.rs` | Does an entity the London Gazette reports as in liquidation still hold a live contract award? | `InLiquidation` rows (source `london_gazette`) intersected with `AwardedTo` | NamesNodes | Chips |
| 13 | `property-nodes` | `property_nodes.rs` | Does the graph hold Property nodes from CCOD or OCOD, checked against each producer's run marker? | Property nodes and the per-producer `enrichment_runs` markers | NamesNodes | Chips |
| 14 | `phase20-ec-donations` | `electoral_commission_donations.rs` | Do Electoral Commission donation records exist for entities that are also in the procurement graph? | `electoral_commission` identity links and `DonatesTo` edges | NamesNodes | Chips |
| 15 | `phase20-council-registers` | `council_registers_political_actors.rs` | Does the graph hold political-actor evidence that can be attributed to council registers, kept separate from Electoral Commission and planning-identity data? | The `council_registers` run marker (checked first), `PoliticalActor` nodes and `DeclaresInterest` edges | AggregateOnly | Absence |
| 16 | `osint-confirmed-intel` | `osint_confirmed_intel.rs` | Are there confirmed or auto-linked identity resolutions, and do any carry a high-risk resolution type? | Confirmed `identity_links` | NamesLinksOnly | Chips |
| 17 | `osint-analyst-queue` | `analyst_review_queue.rs` | How many identity links at 0.70–0.84 confidence are waiting for analyst sign-off, and which should be cleared first? | `pending_review` `identity_links` (full read, with a top-10 display sample) | NamesLinksOnly | Chips |
| 18 | `osint-new-node-types` | `osint_new_node_types.rs` | Does the graph contain any of the newer OSINT node types (Psc, Charity, Property, OffshoreEntity, InLiquidation)? | Five attributed node-type counts | NamesNodes | Chips |
| 19 | `osint-risk-edges` | `disqualified_director_edges.rs` | Do disqualified-director edges intersect with other OSINT signals (Controls, OffshoreLink, donations, TrusteeOf)? | `DisqualifiedFrom`, `Controls`, `OffshoreLink`, `DonatesTo` and `TrusteeOf` edges, plus identity links | NamesNodes | TypedEdge |
| 20 | `land-flip-candidates` | `land_flip_candidates.rs` | Do any gate-1 survivors also pass gate-3 corroboration, split by structural CCOD-versus-individual capability? | The `land_flip_runs:current` marker and the persisted capability split | AggregateOnly | Absence |
| 21 | `donation-contract-correlation` | `donation_contract_correlation.rs` | Does a donation from an entity fall within 730 days (either side) of a contract award to that entity from a party-controlled LPA? | `electoral_commission` identity links, the party map in `data/party_authority_control.json`, and contract awards | AggregateOnly | Absence |
| 22 | `opencorporates-cross-jurisdiction` | `opencorporates_cross_jurisdiction.rs` | Does an OpenCorporates cross-jurisdiction lookup confirm foreign-registered entities tied to the procurement graph? | Three config gates (`OcProbeGates`) and the run marker's `candidates_found` | AggregateOnly | Absence |

### Verdicts each probe can emit

Every probe builds its measured verdicts with `Verdict::classify`, which yields `Hit`, `Weak`,
`Clear` or `NotApplicable` depending on the counts passed in. Every probe also has at least one
`Verdict::data_gap` path. Only two probes ever construct `Verdict::blocked` directly:

- `address-cluster-exact` returns `Blocked` when it finds no shared address **and**
  `identity-integrity` has already set `Strategy.identity_blocked`. In that case it cannot
  trust its own count of distinct applicants.
- `opencorporates-cross-jurisdiction` returns `Blocked` and names the unmet config gate when
  the adapter is disabled, open-data use is not confirmed, or no API key is configured. It
  records only `has_api_key: bool`, so the key string never reaches a report.

Any probe can come back as `Skipped` under gated dispatch (see [Gating](#gating-and-dependency-declaration)).

`osint-disqualified-directors` and `osint-evasion-alerts` share
`disqualification_chain::verdict_for_cause`. They are the only two probes that fill
`Finding.cause`:

| `Cause` | Verdict |
|---|---|
| `NoOfficerNodes` | `DataGap` |
| `AdapterNeverRan` | `DataGap` |
| `NoRegisterOverlap` | `Clear` |
| `LinksNotElevated` | `NotApplicable` (sub-threshold candidates held back by the status gate) |
| `NoLiveBan` | `Clear` |

`ward-concentration` is the only probe that sets `Finding.remediation_hint`
(`RemediationHint::WardsUnattributed(n)`).

## Verdict model

Verdicts are defined in `crates/schema/src/types/verdict.rs`. There are seven variants:

| Variant | Carries | Meaning |
|---|---|---|
| `Hit { coverage, matches }` | `Coverage` | The whole capable population was evaluated and `matches` reached the probe's hit threshold. |
| `Weak { coverage, matches }` | `Coverage` | The whole capable population was evaluated and there were some matches, but fewer than the threshold. |
| `Clear(coverage)` | `Coverage` | The whole capable population was evaluated and there were no matches. |
| `NotApplicable { coverage, reason }` | `Coverage` | Only part of the population could structurally be evaluated. `reason` explains the exclusion. |
| `DataGap { missing_input }` | — | A required input was missing, so nothing was measured. |
| `Blocked { prerequisite }` | — | An unmet prerequisite stopped the probe from running. |
| `Skipped { cause }` | `SkipCause` | The gate predicate decided the probe should not run. |

The model enforces these invariants:

- **`Coverage` can only come from `Verdict::classify(population, capable, matches,
  hit_threshold, not_applicable_reason)`.** Its fields are private, so another crate cannot
  build a `Hit`, `Weak`, `Clear` or `NotApplicable` with made-up counts. `classify` panics if
  `capable > population`. If `capable < population` it always returns `NotApplicable`.
  Otherwise it returns `Clear` for `matches == 0`, `Hit` for `matches >= hit_threshold`, and
  `Weak` for anything in between.
- **A `Clear` over an empty population must not happen.** Where a probe's population can be
  zero, it short-circuits to `DataGap` before it calls `classify(0, 0, …)`.
- **There is no `Deserialize`.** `Verdict`, `Coverage`, `Finding`, `RecordRef` and their
  related types derive `Serialize` only, so JSON cannot produce a verdict that breaks the
  invariants.
- **`SkipCause` can only be built by `Verdict::skipped_dependency(upstream, &upstream_verdict)`
  or `Verdict::skipped_tier_aggregate(tier, probes_checked)`.** `SkipCause::describe()` is the
  single place the explanatory sentence is written.

`VerdictKind` is a fieldless discriminant of `Verdict`, used by the gate predicate.

A `Finding` (in the same file) contains:

| Field | Type | Notes |
|---|---|---|
| `probe` | `String` | The probe id from the catalogue. |
| `verdict` | `Verdict` | `population()` and `capable()` are always read from this field and never stored separately. |
| `summary` | `String` | The human-readable measurement summary. |
| `cause` | `Option<Cause>` | Set only by the two disqualification-chain probes. |
| `severity` | `Severity` | `Critical` / `High` / `Medium` / `Low` / `Info`. `rank()` gives a sort order. |
| `evidence` | `Vec<EvidenceRow>` | Evidence text plus typed pivot targets. |
| `attributions` | `Vec<SourceAttribution>` | The source and count behind each attributed figure. |
| `remediation_hint` | `Option<RemediationHint>` | See above. |

Two per-probe constants, `tier()` and `hypothesis()`, stay on the `Probe` trait rather than on
`Finding`. `run::execute` joins them onto each `Finding` in a `report::ReportEntry`, together
with `motif_kind` and `population_source`.

### Population provenance

`population_source.rs` places every registered probe in exactly one of two tables:

- **`CHECKED_PROBES` (10 probes)**: `precomputed-alerts`, `identity-integrity`,
  `address-cluster-exact`, `geographic-concentration`, `ward-concentration`, `temporal-burst`,
  `gazette-insolvency-signals`, `property-nodes`, `osint-new-node-types` and
  `phase20-council-registers`. The oracle (`urban_integrity_core::persistence::oracle`)
  recomputes each probe's declared population on its own from raw `graph_events`.
- **`EXCLUDED_PROBES` (12 probes)**: each entry has an `ExclusionCategory`
  (`IdentityLinksSourced`, `VerdictThreshold`, `SemanticMismatch` or `MarkerArithmetic`) and a
  reason that cites its source.

`execute` calls `population_source::for_probe` for every probe that ran and attaches a
`PopulationSource` to each entry:

- `OracleCorroborated { oracle_count }` for a checked probe whose verdict carries `Coverage`.
- `SelfReported { reason }` in every other case: excluded probes, `DataGap` and `Blocked`
  verdicts, and `Skipped` entries.

`verify_registered_probes_covered` bails if the two tables do not exactly partition the
registered battery. This check runs on every `execute`.

## Gating and dependency declaration

`execute` always calls `graph::validate_dependency_graph` first, before any probe runs and in
**both** dispatch modes. Validation fails loudly on any of the following:

- a `depends_on()` entry that names a probe which is not registered;
- a dependency cycle (a hand-written Kahn's algorithm, with registration order as the stable
  tie-break);
- a dispatch order in which a Tier-2 probe comes before any Tier-1 probe.

Once validation passes, the dispatch mode is chosen:

- **Gated (default).** `graph::run_gated` dispatches the probes in topological order. For each
  probe, `gate()` checks two rules:
  1. Every probe named in `depends_on()` must have returned `Hit` or `Weak`. If one did not,
     the result is `Verdict::skipped_dependency`.
  2. A `Tier-2` probe runs only if **at least one** `Tier-1` probe returned `Hit` or `Weak`.
     If none did, the result is `Verdict::skipped_tier_aggregate("Tier-1", n)`.

  A skipped probe is never run. It gets a synthesized `Finding` with the summary
  `Skipped — {cause}` and severity `Info`. Findings come back in registration order, whatever
  order they were dispatched in.
- **Full battery.** `crate::run_probes` runs all 22 probes unconditionally, in registration
  order. That loop never branches on a verdict. The mode is selected by
  `RunOptions { full_battery: true }`, which the CLI exposes as `probe --full-battery`.
  `probe-oracle` always uses it.

**No production probe overrides `depends_on()` today.** The default returns `&[]`, so the only
gating that currently applies in practice is the Tier-1 → Tier-2 rule. The trait doc comment
sets two conditions for declaring an edge:

1. The upstream probe must be able to return `Hit` or `Weak`. Some probes can only ever return
   `Clear` or `DataGap`, and a dependent of one of those would be skipped on every corpus.
2. The dependency must be on the upstream's *signal*, not on the quality of its data.
   Data-quality preconditions belong in `Strategy`. For example, `identity-integrity` sets
   `identity_blocked` and `address-cluster-exact` reads it.

### Shared `Strategy`

`Strategy` (`lib.rs`) is a mutable struct passed through every probe in a run:

| Field | Written by | Read by |
|---|---|---|
| `identity_blocked` | `identity-integrity` | `address-cluster-exact` |
| `osint_active`, `confirmed_link_count`, `pending_review_count` | `identity-link-registry` | later Tier-2 probes, e.g. `osint-analyst-queue` |
| `enrichment_runs` | lazily, by `Strategy::enrichment_runs(db)` | any probe that needs to know whether an adapter ran |
| `notes` | — | report writer |

Because of these reads and writes, registration order matters. `build_battery` puts
`identity-integrity` at position 2 and `address-cluster-exact` at position 4 so that the flag is
written before it is read. The `enrichment_runs` memo reads the table once per run. If that read
fails, it degrades to an empty map, and the probe that asked then takes its own `DataGap` path.

## Evidence model: `RecordRef`

Each `Finding.evidence` entry is an `EvidenceRow { text, refs: Vec<RecordRef> }`. A
`RecordRef { kind, id }` is a typed pivot target. `RecordKind` has these variants:

| Kind | Id shape | Pivot command (`report::evidence::pivot_command`) |
|---|---|---|
| `Entity` | a graph node id, e.g. `GB-COH:…`, `director:…`, `ward:…` | `urban-integrity-cli entity <id>` |
| `Contract` | a `CF:…` or `planning:…` node id | `urban-integrity-cli entity <id>` |
| `Location` | a `location:…` or `LOC:…` node id | `urban-integrity-cli entity <id>` |
| `CompanyNumber` | a bare Companies House number | `urban-integrity-cli verify --company-number <id>` |
| `IdentityLink` | an `identity_links` record id | `urban-integrity-cli links show <id>` |

The battery itself only emits `Entity` and `IdentityLink` refs. `Contract` and `Location` are
built by the workbench entity-profile mapper.

Each probe states the reach of its evidence through `Probe::evidence_scope()`, which has **no
default**:

| `EvidenceScope` | Meaning | Probes |
|---|---|---|
| `NamesNodes` (12) | At least one `RecordRef` is a graph node id | `precomputed-alerts`, `address-cluster-exact`, `ward-concentration`, `temporal-burst`, `identity-link-registry`, `osint-disqualified-directors`, `psc-beneficial-ownership`, `gazette-insolvency-signals`, `property-nodes`, `phase20-ec-donations`, `osint-new-node-types`, `osint-risk-edges` |
| `NamesLinksOnly` (2) | Refs are `IdentityLink` only | `osint-confirmed-intel`, `osint-analyst-queue` |
| `AggregateOnly` (8) | No `RecordRef`; the evidence text gives counts only | `identity-integrity`, `detector-readiness`, `geographic-concentration`, `osint-evasion-alerts`, `phase20-council-registers`, `land-flip-candidates`, `donation-contract-correlation`, `opencorporates-cross-jurisdiction` |

`urban_integrity_probe::battery_evidence_coverage` builds this three-way split from the live
battery. The workbench entity profile uses it to say how many probes it actually searched for
findings about an entity. `crates/probe/tests/findings_coverage.rs` checks each declaration
against the probe's own source.

`Probe::motif_kind()` (also no default) chooses how the finding board draws a probe's card:
`Star`, `TypedEdge`, `RankedBars`, `Chips` or `Absence`. Every `AggregateOnly` probe answers
`Absence`. `crates/probe/tests/motif_kind_coverage.rs` pins the answer for each probe.

Probes that report OSINT counts wrap them in `attributed::Attributed`, through
`count_for_source`, `count_across_producers` or `from_rows`. That wrapper refuses to credit a
count to a single named source when more than one registered producer could have written the
rows and there is no discriminator. The producers end up in `Finding.attributions`.

## Running the battery

### CLI: `probe`

```bash
urban-integrity-cli --analysis <slug> probe [--output <path>] [--json <path>] [--full-battery]
```

`crates/cli/src/commands/probe.rs` loads `AppConfig` once. From that single load it opens both
the analysis connection and the `_catalog` connection (the latter used only by
`identity-integrity`), resolves the analysis database name, and reads `OcProbeGates` from the
config. It then calls `run::execute`. The command:

- prints a table with the columns Probe / Verdict / Summary (for example `HIT (pop/capable,
  matches=n)` or `SKIPPED — <cause>`);
- writes the Markdown report to `--output`, defaulting to `evidence-report.md` in the current
  directory;
- when `--json` is given, also writes every `Finding` as pretty-printed JSON.

It attempts both files before reporting a failure, and it creates missing parent directories.
Leaving out `--analysis` targets the default (`main`) analysis.

The CLI `probe` does **not** write `workbench_report:latest`. A run from the CLI does not show up
on the workbench's analysis page. To get it there, use the workbench's own Run button.

### CLI: `probe-oracle`

```bash
urban-integrity-cli --analysis <slug> probe-oracle [--output <path>]
```

This command runs the full, ungated battery and then compares populations for the probes in
`CHECKED_PROBES`:

1. For each checked probe, it recomputes the population independently and prints a comparison
   table that sets the declared population beside the oracle's count. A probe whose verdict was
   `DataGap` or `Blocked` shows no declared value, so it can never appear as a false
   disagreement.
2. It prints a coverage table naming every excluded probe, its category and its reason.

Per-probe tolerance is set in `PROBE_DELTA`. The table is empty today, so every comparison
expects an exact match.

The command resolves the analysis database before and after the battery runs and bails if the
two differ. If any probe disagrees with the oracle, it points to `docs/ADJUDICATIONS.md`.
`--output` writes a copy of both tables with the terminal styling stripped.

### Workbench

The workbench (`urban-integrity-cli workbench`) runs the same `run::execute`, gated
(`full_battery: false`), inside its own process. Its routes are registered in
`crates/workbench/src/boot.rs`:

| Method | Path | Behaviour |
|---|---|---|
| `POST` | `/api/analysis/run` | Runs the battery. Only one run can be in flight: a concurrent POST gets `409`. On success it caches the result and returns the `ReportView`. On failure it keeps the previous report and returns `500`. |
| `GET` | `/api/analysis/report` | Returns the latest cached `ReportView`, or `404 {"error":"no run yet"}`. |
| `GET` | `/api/analysis/report.md` | Returns the Markdown report as rendered at run time. The battery is not re-run. |

The **Run** button on the workbench analysis page (`crates/workbench-web/src/analysis_page.rs`)
posts to `/api/analysis/run`.

After each successful run, `report_persistence::persist_report` upserts the record
`workbench_report:latest` with the fields `view` (a `ReportView` as a JSON object), `markdown`
and `run_at` (a `datetime`). `boot` reads that record back through `load_report`, so the last
report survives a restart. Persistence is best-effort: a failed write is logged as a WARN and
never fails the run.

`ReportView` and `ProbeView` are defined in `crates/workbench-dto/src/report_view.rs`. They are
persisted, so any new field on them needs `#[serde(default)]`, or older stored reports will stop
deserializing.

The run also stores the raw `ReportEntry` values in `WorkbenchState.last_findings`. The entity
profile's findings section reads from there.

### In-process (library)

```rust
use urban_integrity_probe::run::{execute, OcProbeGates, RunOptions};
use urban_integrity_probe::report::render_evidence_report;

let result = execute(
    &db,                              // analysis connection
    catalog,                          // `_catalog` connection
    db_name,                          // resolved analysis database name
    OcProbeGates::from_config(&config),
    RunOptions { full_battery: false },
)
.await?;
let markdown = render_evidence_report(&result.entries, &result.strategy);
```

`execute` returns a `BatteryResult { entries: Vec<ReportEntry>, strategy: Strategy }`, one entry
per registered probe, in registration order.

## Report output

`report::render_evidence_report` (`crates/probe/src/report/mod.rs`) builds the Markdown report
in a fixed order:

1. `# UrbanIntegrity Evidence Report` followed by a `Generated: <RFC3339>` line
2. `## Bottom Line Up Front` (`bluf.rs`)
3. `## Verdict Legend` (`legend.rs`)
4. `## Skipped Probes` (`skipped.rs`), which appears early so gated-out probes are hard to miss
5. `## How the Search Adapted` (`adapted.rs`)
6. The per-verdict detail groups from `DETAIL_GROUPS`, in this order: **Confirmed Findings**
   (Hit), **Suggestive Patterns** (Weak), **Data-Integrity Gaps** (DataGap and Blocked), **Not
   Applicable**, and **Verified Negative Findings** (Clear). Each entry has a `### <probe id>`
   heading, the probe's hypothesis, its summary, and its evidence rows with
   `-> pivot: <command>` lines. Clear entries also get a *Population source* line.
7. `## Full Probe Ledger` (`ledger.rs`), which includes a Provenance column
8. `## Contradiction Checks` (`contradictions.rs`)
9. `## Recommended Remediation` (`remediation.rs`)

The workbench's `report_view::from_battery` walks the same `DETAIL_GROUPS` table, so the web
view and the Markdown report cannot group findings differently.
`crates/probe/tests/golden_report.rs` pins the exact Markdown output, minus the `Generated:`
line, for a fixture that covers every verdict variant.

## Testing against the planted fixture

`crates/test-support/src/planted.rs` provides a synthetic graph of about 82 nodes and a
`Manifest` of expected answers written as literals. The manifest includes
`evidence_refs: Vec<ExpectedProbeRefs>`, which gives the expected total `RecordRef` count for
selected probes. `crates/probe/tests/planted_evidence_refs.rs` seeds the fixture into a
throwaway `TestNs` namespace, runs each of those probes, and compares the counts against the
manifest.

To seed a disposable analysis by hand, use:

```bash
urban-integrity-cli --analysis <slug> fixture seed --planted --reseed
```

`scripts/toy-analysis.sh` wraps the full sequence: create the analysis, seed it, project it,
and run the probe battery with its Markdown output written to `data/toy-evidence-report.md`.

DB-gated probe tests need `URBAN_DB_TESTS=1` and a reachable SurrealDB instance. See
[TESTING.md](TESTING.md).

## Adding a new probe

1. **Create the module.** Add `crates/probe/src/<name>.rs` and declare it with `pub mod <name>;`
   in `lib.rs`.
2. **Implement `Probe`.** The required methods are `name()` (a kebab-case id), `tier()`
   (exactly `"Tier-1"` or `"Tier-2"`), `hypothesis()` (one sentence), `evidence_scope()`,
   `motif_kind()` and `async fn run(&self, db, st) -> Finding`. `depends_on()` is optional and
   defaults to none. `run` cannot fail. Catch query errors inside it and return
   `Verdict::data_gap(..)` or `Verdict::blocked(..)` instead.
3. **Report honest counts.** Build verdicts with `Verdict::classify(population, capable,
   matches, hit_threshold, reason)`. Return `DataGap` when the population is zero. Wrap OSINT
   counts in `attributed::Attributed`. Attach a `RecordRef` to every evidence row that names a
   record the reader can pivot to.
4. **Register it.** Add the probe to `run::build_battery`, placing every Tier-1 probe before
   every Tier-2 probe (graph validation rejects interleaving). Also check which `Strategy` fields
   the probe reads or writes, and place it after the probes that write them.
5. **Classify its population.** Add an entry to either `CHECKED_PROBES`, with an
   `OracleCheckKind`, or `EXCLUDED_PROBES`, with a category and a reason that cites its source,
   in `population_source.rs`. `execute` refuses to run until every registered probe appears in
   exactly one of the two tables.
6. **Update the pinned tables.** Add the probe to
   `crates/probe/tests/findings_coverage.rs` and `crates/probe/tests/motif_kind_coverage.rs`.
   If it emits refs that the planted fixture exercises, add an `ExpectedProbeRefs` literal to
   the manifest in `crates/test-support/src/planted.rs`.
7. **Run the tests.** Run `cargo test -p urban-integrity-probe` and repeat it with
   `URBAN_DB_TESTS=1` so the DB-gated tests actually execute.
