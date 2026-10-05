<!-- generated-by: gsd-doc-writer -->
# UrbanIntegrity Engine

<!-- GSD-MANAGED-DOC: readme -->

> **Real-time streaming audit engine for UK public procurement corruption detection.**  
> Intersects high-pressure infrastructure demand with high-risk corporate behaviour — flagging Phoenix companies, address clusters, nepotism bridges, and pre-permission land flips *before* contracts are signed.

[![Rust](https://img.shields.io/badge/Rust-1.96.0%20(MSRV%201.87+)-orange)](https://rust-lang.org)
[![SurrealDB](https://img.shields.io/badge/SurrealDB-3.1.0--beta.2-blue)](https://surrealdb.com)
[![License](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue)](LICENSE)

> **⚠️ Work in progress — this software makes mistakes.** Its alerts, scores and reports are leads for
> qualified review, never evidence of wrongdoing. Do not accuse any person or organisation on the basis of
> its output unless a qualified fraud investigator is involved and you have taken legal advice first. See
> [DISCLAIMER.md](DISCLAIMER.md).

---

## What It Does

UrbanIntegrity ingests live data from UK public sources and builds a **Heterogeneous Information Network (HIN)** graph:

| Source | Data | Protocol |
|--------|------|----------|
| [Companies House](https://stream.companieshouse.gov.uk) | Entity registrations, officer appointments, dissolutions | Streaming API (long-lived HTTPS) + REST backfill |
| [Contracts Finder](https://www.contractsfinder.service.gov.uk) | OCDS-compliant public procurement awards | REST polling |
| Planning Portals (Idox/Swift APAS/Arcus/WFS) | Infrastructure demand signals | Scrapling sidecar (FastAPI :8765) |
| [HM Land Registry](https://www.gov.uk/government/collections/price-paid-data) | Property ownership (CCOD / OCOD) + Price Paid Data (PPD) | Bulk CSV / TXT (OGL v3) |
| [FCA Mutuals Public Register](https://mutuals.fca.org.uk) | Registered societies (co-ops, community benefit societies, housing associations) | Bulk CSV (unauthenticated) |

As edges arrive, **motif detectors** scan the graph for corruption patterns:

- **Phoenix Loop** — Active entity shares officers with a dissolved predecessor → risk of asset stripping / identity laundering
- **Address Cluster** — Multiple bidders share a registered address → shell company ring (contracting authorities, known formation-agent addresses, and the CH default address are excluded to suppress false positives)
- **Nepotism Bridge** — Short path from a political actor to a contract officer → undue influence
- **Pre-Permission Land Flip** — A property at a planning-application address changes hands near the application window, corroborated by an exact-identifier link from the applicant's net → speculative land flip

Detections, probe verdicts and the graph itself are explored either through the CLI or through the
**analyst workbench** — a browser UI (Leptos, compiled to WebAssembly) served by the `workbench` subcommand
over a read-only replay of the analysis. See [Analyst Workbench](#analyst-workbench).

Every detection produces a `MotifResult` carrying a `P_c` score. `P_c` is a weighted blend of the three
motif signals (`compute_p_base` — phoenix 0.50, proximity 0.30, clustering 0.20), T-Digest normalised across
the corpus, then optionally uplifted by the ward's deprivation rank. All of that lives in
`crates/detector/src/scoring/` (`formula.rs`, `imd.rs`, `registry.rs`) — **not** in the `stats` crate, which
is an empty stub (see the crate table below).

**Deprivation weighting is implemented, and its index does not ship.** The uplift takes a fraction of the
alert's remaining headroom rather than multiplying the base score:
`p_final = p_base + (1 - p_base) * IMD_WEIGHT * deprivation`, with `IMD_WEIGHT = 0.3`. The multiplicative
form it replaced saturated: enabling a real index pushed all 1,231 weighted alerts onto exactly 1.0,
inverting the ordering the weighting exists to sharpen. The headroom form cannot exceed 1.0 by construction,
so it is a genuine tie-break — an alert at 0.85 in the most deprived Welsh ward reaches 0.888, still below an
alert at 0.95 in the least deprived. `config.toml` points `[detector].imd_csv_path` at
`data/wimd_2019_wards.csv`, but **`data/*` is gitignored**, so no index arrives with a clone. Until you
regenerate one, `serve` logs `IMD index load failed` and every alert carries `m_env = 1.0` with a null
`imd_rank` — un-weighted, but stated rather than silent:

```bash
python3 scripts/build_deprivation_ward_index.py --nation wales --lad <W06… code>  # one authority (WIMD 2019)
python3 scripts/build_deprivation_ward_index.py --nation wales --all              # every Welsh ward
python3 scripts/build_deprivation_ward_index.py --nation england --lad <E0… code>  # name the LADs
# --nation alone is rejected for wales/england/scotland: an analysis is ward-scoped and
# --all would page the entire national postcode directory. northern-ireland needs neither.
```

**One nation per file.** The UK publishes four non-comparable deprivation indices — England IMD 2019
(32,844 LSOAs), Wales WIMD 2019 (1,909), Scotland SIMD 2020 (6,976), NI NIMDM 2017 (462 wards). All rank
1 = most deprived, but a rank of 500 is the 26th percentile in Wales and the 1.5th in England. The loader
requires an `IMD Denominator` column and rejects a file whose rows disagree about it; a startup jurisdiction
preflight refuses an index belonging to a different nation from the analysis's wards, because the wrong
nation's index loads cleanly, matches zero wards, and leaves the weighting inert while appearing configured.

### Data-Integrity Policy: Exact-Match-Only Entity Resolution

**As of 2026-06-13 all in-application fuzzy and phonetic name matching has been removed.** The former `resolver.rs` module is deleted and the `strsim` and `rphonetic` dependencies are dropped from the `enricher` crate. Cross-registry identity links are now created **only from exact-identifier joins** — exact Companies House `company_number` matches and exact normalised-name equality. This eliminates the false-positive classes (prefix-bias, single-word phonetic collisions) that name-only matching produced. The one exception is OpenSanctions, which delegates scoring to an **external, self-hosted yente service**; UrbanIntegrity itself performs no in-process fuzzy scoring.

### Detectors Against Live Data

**The officer and PSC layers were empty for most of this project's life.** Both were populated on 2026-08-03:
a Companies House REST officer backfill (~18h over 14,135 companies) took the `officer` table
**0 → 21,923** with 22,807 `Directs` edges, and `enrich --source ch_psc` took `psc` **0 → 17,298** and
`controls` **82 → 17,245**. Re-measured against the live Anglesey projection on 2026-08-25:
`officer` 22,703, `psc` 17,867, `controls` 17,827, `directs` 24,658. Officer nodes were subsequently re-keyed from the per-company
`OFF:{company}-{NAME}` form to the canonical `director:{slug}` form, collapsing genuine same-person
duplicates; the 94 fusions that could not be defended on the available evidence were split back apart.

That backfill is now a first-class CLI subcommand, `backfill officers`, and it keys on the identity evidence
Companies House actually publishes — the officer id and the `{month, year}` date of birth — rather than on
the name alone. `--audit` checks the `director:` keys already in the graph and records an
`officer_key_collision` anomaly for each key that conflates people CH says are different; `--split-collisions`
re-keys the uncorroborated ones onto `-o{officer_id}` and leaves events no CH evidence attributes exactly
where they are, reported rather than guessed at. Note what the evidence supports and what it does not: two
officer ids that **agree** on a date of birth are one human that CH has fragmented, not a collision, and CH
publishes no date of birth at all for company secretaries — so a name-only key that fuses two secretaries
remains unfalsifiable from CH data.

**NepotismBridge** has been exercised live on a Sandwell analysis. After linking Sandwell councillors to Companies House officer records via `scripts/pa_link_officers.py`, the detector **flagged 528 NepotismBridge patterns** across **51 of 72 Sandwell councillors** — each councillor's `PA:{slug}` node directly connected to an officer node at BFS hop-distance 1. A flag of this kind means a councillor and a company officer share an identity key in the graph; it is a structural pattern for review, not evidence of any wrongdoing (most such links have entirely ordinary explanations). **That result is not reproducible from the maintainers' current catalogue**, which registers only
the Anglesey analysis (active) and a retired end-to-end test copy of it; no Sandwell analysis exists there,
and the Anglesey dataset holds **zero** NepotismBridge alerts. Re-measured 2026-08-25, its corpus is 1,947
`MotifDetected` events — 1,807 `AddressCluster`, 138 `property_accumulation_anomaly` and 2 `PhoenixLoop`,
one of which is withdrawn, so 1,946 are live. It holds no land-flip, evasion or donation-correlation alerts
either. `urban-integrity-cli alerts list --motif nepotism` will therefore return
nothing unless you rebuild an analysis containing councillor data. Treat the Sandwell figures as a record of
a past run, not as something the current catalogue can show you.

**PhoenixLoop holds two alerts; one is live, and it is now reachable.** (An alert is a flagged pattern —
a lead for review, not a finding that anything improper happened.) For most of the project's life the
motif was structurally unable to fire at all: officers were keyed per company (so no officer node was ever
shared by two companies), and the CSR snapshot exposed outgoing adjacency only, while the motif needs to
traverse both hops. Both were fixed in August 2026 (`CsrSnapshot` now carries `reverse_adj` alongside the
forward index), at which point Phoenix fired twice — on 2026-08-10 (21:14Z) and 2026-08-11 (13:42Z). The
first was **withdrawn**: it rested on a name-only officer merge. The second is **still standing** —
`PhoenixLoop`, score 0.85, an active company sharing one `director:` key with a failed Companies House
company, `withdrawn_at` null. (Named subjects are deliberately not reproduced in this README; query the
alert with `alerts list --motif phoenix`.)

That alert was invisible through every analyst-facing path until 2026-08-18, which is why this file (and
the project notes) recorded Phoenix as holding nothing. Three defects in one read path: `alerts list` ordered by
`timestamp DESC` rather than score, so a 0.85 Phoenix sat beneath 1,807 later `AddressCluster` rows; the
motif filter ran inside a `limit × 10` recency window rather than over the corpus; and the limit was
silently clamped to 1,000, so `--limit 2000` returned exactly 1,000 with no signal. All three are fixed in
`5a95ba9` — `alerts list` now orders by score descending (with `detected_at` then id as total-order
tie-breaks), scans the full `MotifDetected` set through the paginated `scan_events_by_kind`, and reports
`showing N of M`. `alerts list --motif phoenix` returns the alert. Live corpus, re-measured 2026-08-25:
1,946 live alerts, 1,947 including the withdrawn one. The surviving Phoenix row still reads
`risk_score 0.85`, `m_env 1.0`, `imd_rank null`, `withdrawn_at null`.

Two caveats remain on the detector itself. Both alerts fired while `serve` built its `DetectorConfig` from
literals — a hardcoded 730-day window against `config.toml`'s declared 180 — so the surviving alert's pair
is 190 days apart and could not have fired under the configured policy. `serve` now reads the `[detector]`
block (`detector_config_from`), and `PhoenixConfig::default()` is 180 days. And Phoenix's failed-company
universe is narrow: the Companies House stream only emits companies that *change*, so the graph's dissolved
corpus is one bulk-dissolution day wide (widen it with `backfill cessation`). The `date_published` gap that
blocked the `|award_date − dissolution_date| ≤ 180 days` test is largely closed — 21.0% of the ingested
Contracts Finder corpus lacked it when re-measured 2026-08-25 (1,264 of 6,014), down from 89% before the
`release.date` fallback in `cf/mapper.rs` and `scripts/cf_backfill_dates.py`.

## Status

| Release | Date | What landed |
|---------|------|-------------|
| v1.0 | 2026-05-13 | Three live data sources, full motif detection, CLI/gRPC/GraphQL wired to SurrealDB, P99 = 2.5 µs at 10k concurrent reads |
| v2.0 | 2026-05-27 | CLI analyst platform — `entity`, `alerts`, `report`, `graph-path`, `verify` and the GraphQL API, so journalists, campaigners and transparency officers can query corruption risk, export cited reports, and audit counterparties before signature |
| v3.0 | 2026-06-11 | OSINT enrichment layer — multi-registry entity resolution across 10+ open data sources. Entity resolution hardened to exact-match-only in a follow-up data-integrity pass (2026-06-13) |
| v4.0 | 2026-06-20 | Data activation — CLI graph-mutation gap closed, HM Land Registry property layers (OCOD overseas-owned + CCOD UK-company-owned), the Price Paid Data staging adapter, and a three-gate Pre-Permission Land Flip detector with an analyst review queue (`flips`) |
| v5.0 | 2026-07-26 | Analysis lifecycle — per-analysis SurrealDB databases with a `_catalog` registry, `analysis create`/`list`/`remove`, a global `--analysis` selector on every command, and `--from` baseline cloning via `surreal export \| import` |
| v6.0 | 2026-08-23 | Data quality & compliance — three-state planning-applicant provenance, Phoenix enablement (officer re-keying to `director:{slug}`, reverse adjacency, config-driven detector settings), the `anomalies` surface, four-nation deprivation indices with a jurisdiction preflight, canonical `GB-OCOD:` owner keys, and both destructive purges re-enabled behind a typed-digest handshake and a counted delete |
| v7.0 | 2026-08-31 | Rust probe port & verdict correctness. All 22 forensic probes from `scripts/analyst_probe.py` now run as `urban-integrity-cli probe`, reading exclusively through `crates/core/src/persistence`. The namesake defect — a probe reporting `CLEAR` over a population it never measured — is closed by a **construction invariant**, not a convention: `Coverage`'s fields are private and reachable only through `Verdict::classify`, so a fabricated `Coverage { population: 5, capable: 3 }` is a compile error, and `Clear` is unreachable unless `capable == population && matches == 0` |
| v8.0 | 2026-09-04 | Independent verification & verdict gating — ward-reachable export (`export-graphml --reachable-from-wards`); an independent population oracle (`persistence::oracle`) and the `probe-oracle` comparison command; `PopulationSource` carried on every report entry so a `Clear` over an oracle-corroborated population reads differently from one over a self-reported population; the three Python-vs-Rust probe disagreements settled in [`docs/ADJUDICATIONS.md`](docs/ADJUDICATIONS.md); and verdict-gated probe orchestration (`Verdict::Skipped`, `--full-battery`, a "Skipped Probes" report section) |
| v9.0 | in progress | Fraud analyst workbench — the `workbench` subcommand and the `probe`, `workbench`, `workbench-dto` and `workbench-web` crates: analysis picker and status, in-browser probe runs and evidence-report download, alert/motif triage with withdraw, entity search and profiles, saved investigations, a Cosmograph graph canvas over the ward-reachable population, and in-UI graph algorithms (see [Analyst Workbench](#analyst-workbench)) |

v6.0 closed with three things explicitly *not* delivered, recorded rather than quietly dropped: **retention
automation deferred** (on indefinite hold pending a live external deployment), **land-flip gate-3
corroboration explicitly unmet** (gate-3 corroborates against the corporate and overseas ownership
registers, and 94.6% of gate-1 survivors are private individuals with nothing to resolve — the zero is
substantially correct behaviour on this corpus, which is why it must not be recorded as met), and
**evidence-report presentation work withdrawn** before it was specified.

**Verdict gating.** The battery gates on verdicts instead of always running all 22 probes: a Tier-2 probe
runs only if at least one Tier-1 probe returned `HIT`/`WEAK`, and a skipped probe is reported with the
upstream verdict that caused the skip. The one explicit dependency edge gating first shipped with
(`address-cluster-exact → identity-integrity`) was **removed on 2026-09-03**: `identity-integrity` can never
return `HIT` or `WEAK`, so the gate could never open on any corpus, and a `Clear` there means the
precondition is *satisfied*, not failed. It had been silently skipping `address-cluster-exact` — a HIT with
28,282 addresses shared by 2+ distinct applicants when run on the live Anglesey corpus. The shipped battery
now declares **no** dependency edges; gating comes from the Tier-1/Tier-2 rule alone.

Gating is about not reporting a verdict over a population nothing established, not about speed.

> **Note on the OSINT adapters:** several registries originally shipped with name-only fuzzy fallbacks. Those fallbacks were removed on 2026-06-13 — the registry adapters now emit links from exact identifiers only (see *Data-Integrity Policy* above). The two exceptions are external or bounded by design: OpenSanctions delegates scoring to the self-hosted yente service, and the OCOD↔ICIJ overseas-ownership matcher is hard-capped at 0.65 (sub-candidate, investigative-only).

## Architecture at a Glance

```mermaid
flowchart TD
    %% ── External Data Sources ──────────────────────────────────────────
    subgraph Sources["External Data Sources"]
        chApi["Companies House<br/>Streaming API (HTTPS)"]
        cfApi["Contracts Finder<br/>REST / OCDS polling"]
        ppSidecar["Planning Sidecar<br/>Scrapling :8765"]
        hmlr["HM Land Registry<br/>CCOD · OCOD · PPD (OGL v3)"]
        fcaMut["FCA Mutuals Register<br/>bulk CSV (no key)"]
    end

    %% ── crate: ingestor ────────────────────────────────────────────────
    subgraph Ingestor["crate · ingestor"]
        chClient["CH Streaming Client<br/>NDJSON + reconnect cursor"]
        cfClient["CF REST Poller<br/>OCDS mapper"]
        planClient["Planning Client<br/>sidecar bridge"]
        addrNorm["Address Normaliser"]
    end

    %% ── crate: core · EventPipeline ───────────────────────────────────
    subgraph CorePipeline["crate · core — EventPipeline"]
        pipeStages["Validate → Normalise<br/>→ GraphMutation"]
    end

    %% ── crate: core · Graph Engine ────────────────────────────────────
    subgraph GraphEngine["crate · core — Graph Engine"]
        coordinator["GraphCoordinator<br/>Tokio event loop"]
        storage["GraphStorage<br/>DashMap node/edge index"]
        csrSnap["CsrSnapshot<br/>Arc via ArcSwap (read-only)<br/>fwd + reverse adjacency"]
        walChan["WAL Channel<br/>crossbeam-channel"]
    end

    %% ── crate: core · Persistence ─────────────────────────────────────
    subgraph CorePersist["crate · core — Persistence"]
        surrWriter["SurrealDB Writer Loop"]
        checkpt["Checkpoint / Hydrate"]
    end

    surrealDb[("SurrealDB (rocksdb)<br/>ws://localhost:8000")]

    %% ── crate: detector ───────────────────────────────────────────────
    subgraph Detectors["crate · detector"]
        phoenix["Phoenix Detector<br/>asset-stripping loop"]
        cluster["Cluster Detector<br/>shared-address ring"]
        nepotism["Nepotism Detector<br/>political proximity path"]
        scoring["Scoring Engine<br/>detector/src/scoring<br/>T-Digest + deprivation uplift"]
        alertCache["AlertCache<br/>LRU · 1 hr TTL"]
    end

    %% ── crate: enricher ───────────────────────────────────────────────
    subgraph EnricherCrate["crate · enricher"]
        enrichEngine["EnricherEngine<br/>adapter runner"]
        pscAdapt["ChPscAdapter<br/>CH PSC beneficial ownership"]
        disqAdapt["ChDisqualifiedAdapter<br/>CH Disqualified Directors"]
        evasionDet["EvasionDetector<br/>Disqualified Director Evasion"]
        ccodAdapt["LandRegistryCcodAdapter<br/>HMLR CCOD bulk CSV"]
        ocodAdapt["LandRegistryOcodAdapter<br/>HMLR OCOD overseas CSV"]
        ppdAdapt["LandRegistryPpdAdapter<br/>HMLR PPD staging (ppd_transactions)"]
        gazetteAdapt["LondonGazetteAdapter<br/>insolvency notice polling"]
        propAccDet["PropertyAccumulationDetector<br/>property-to-contract ratio anomaly"]
        landFlipDet["LandFlipDetector<br/>3-gate pre-permission land flip"]
        charityAdapt["CharityCommissionAdapter<br/>exact company-number join"]
        opensanctAdapt["OpenSanctionsAdapter<br/>external yente PEP + sanctions screening"]
        icijAdapt["IcijAdapter<br/>Offshore Leaks exact join"]
        ecAdapt["ElectoralCommissionAdapter<br/>political donation CSV (exact CH no.)"]
        councilAdapt["CouncilRegistersAdapter<br/>councillor declarations (Scrapling)"]
        ocAdapt["OpenCorporatesAdapter<br/>cross-jurisdiction lookup (exact CH no.)"]
        mutualsAdapt["MutualsRegisterAdapter<br/>registered societies (GB-MUT:)"]
        planIdAdapt["PlanningIdentityAdapter<br/>applicant ↔ CH exact-name join"]
        donCorDet["DonationCorrelationDetector<br/>730-day revolving-door"]
        idLinks["identity_links table<br/>exact-identifier proposals"]
        flipQueue["land_flip_candidates<br/>analyst review queue"]
    end

    %% ── crate: api ────────────────────────────────────────────────────
    subgraph ApiCrate["crate · api"]
        httpAxum["Axum HTTP :8080<br/>/health · /metrics"]
        gqlEnd["async-graphql<br/>/graphql · /graphiql"]
        grpcEnd["tonic gRPC :8081<br/>IntegrityService"]
    end

    %% ── crate: cli ────────────────────────────────────────────────────
    subgraph CliCrate["crate · cli"]
        cliCmd["report · alerts · anomalies · status<br/>entity · graph-path · serve · ingest-planning<br/>project · backfill · export-graphml<br/>enrich · links · flips · verify<br/>migrate · analysis · fixture<br/>probe · probe-oracle · workbench<br/>purge-orphans · purge-ocod-owners"]
    end

    %% ── crate: probe ──────────────────────────────────────────────────
    subgraph ProbeCrate["crate · probe"]
        battery["22-probe forensic battery<br/>verdict gating · evidence-report writer"]
    end

    %% ── crates: workbench ─────────────────────────────────────────────
    subgraph WorkbenchCrates["crates · workbench / workbench-dto / workbench-web"]
        wbServer["workbench (Axum)<br/>read-only replay · /api/* on :8090"]
        wbDto["workbench-dto<br/>serde-only wire types"]
        wbWeb["workbench-web<br/>Leptos CSR (wasm32) + Cosmograph"]
    end

    ollama["Ollama LLM<br/>localhost:11434 · Gemma4"]

    %% ── Data Flow ─────────────────────────────────────────────────────
    chApi --> chClient
    cfApi --> cfClient
    ppSidecar --> planClient

    chClient --> addrNorm
    cfClient --> addrNorm
    planClient --> addrNorm

    addrNorm -->|IngestEvent| pipeStages
    pipeStages -->|GraphMutation via mpsc| coordinator

    coordinator --> storage
    coordinator -->|GraphEvent| walChan
    coordinator -->|bake every N events / T secs| csrSnap

    walChan --> surrWriter
    surrWriter --> surrealDb
    checkpt <-->|hydrate on start + persist| surrealDb

    imdIdx["data/&lt;nation&gt;_wards.csv<br/>deprivation index (gitignored,<br/>regenerate per nation)"]
    imdIdx -->|ward rank + denominator| scoring

    csrSnap -->|snapshot + delta nodes| phoenix
    csrSnap --> cluster
    csrSnap --> nepotism

    phoenix --> scoring
    cluster --> scoring
    nepotism --> scoring
    scoring --> alertCache

    alertCache --> httpAxum
    alertCache --> gqlEnd
    alertCache --> grpcEnd
    storage --> gqlEnd
    surrealDb --> gqlEnd

    grpcEnd -->|SummarizeAlert| ollama

    cliCmd -->|serve spawns engine| coordinator
    cliCmd -->|report / alerts / entity / graph-path / verify queries| surrealDb
    cliCmd -->|project reprojects WAL → native tables| surrealDb
    cliCmd -->|backfill enriches nodes| surrealDb
    cliCmd -->|export-graphml reads the WAL (or native tables) → GraphML| surrealDb
    cliCmd -->|probe / probe-oracle| battery
    battery -->|reads graph_events| surrealDb
    cliCmd -->|workbench boots| wbServer
    wbServer -->|replay graph_events| surrealDb
    wbServer -->|in-process probe runs| battery
    wbWeb -->|JSON + Arrow IPC over HTTP| wbServer
    wbDto -.-> wbServer
    wbDto -.-> wbWeb
    cliCmd -->|enrich runs OSINT adapters| enrichEngine
    cliCmd -->|links list/show/confirm/reject| idLinks
    cliCmd -->|flips list/show/confirm/dismiss| flipQueue

    hmlr --> ccodAdapt
    hmlr --> ocodAdapt
    hmlr --> ppdAdapt
    fcaMut --> mutualsAdapt

    surrealDb --> enrichEngine
    enrichEngine --> pscAdapt
    enrichEngine --> disqAdapt
    enrichEngine --> ccodAdapt
    enrichEngine --> ocodAdapt
    enrichEngine --> ppdAdapt
    enrichEngine --> gazetteAdapt
    enrichEngine --> charityAdapt
    enrichEngine --> opensanctAdapt
    enrichEngine --> icijAdapt
    enrichEngine --> ecAdapt
    enrichEngine --> councilAdapt
    enrichEngine --> ocAdapt
    enrichEngine --> mutualsAdapt
    enrichEngine --> planIdAdapt
    pscAdapt --> idLinks
    disqAdapt --> idLinks
    ccodAdapt --> idLinks
    ocodAdapt --> idLinks
    charityAdapt --> idLinks
    opensanctAdapt --> idLinks
    icijAdapt --> idLinks
    ecAdapt --> idLinks
    councilAdapt --> idLinks
    ocAdapt --> idLinks
    planIdAdapt --> idLinks
    disqAdapt --> evasionDet
    gazetteAdapt -->|UpdateNode InLiquidation| coordinator
    ccodAdapt -->|AddNode Property + AddEdge OwnsProperty| coordinator
    ocodAdapt -->|AddNode Property + AddEdge OwnsProperty| coordinator
    mutualsAdapt -->|AddNode Entity/Location + AddEdge LocatedAt| coordinator
    planIdAdapt -->|AddEdge Directs on auto-link| coordinator
    ppdAdapt -->|UPSERT ppd_transactions (staging only)| surrealDb
    enrichEngine -->|detect_land_flips| landFlipDet
    landFlipDet -->|3 gates: address + temporal + corroboration| flipQueue
    evasionDet --> alertCache
    propAccDet --> alertCache
    donCorDet --> alertCache
    idLinks --> surrealDb
    flipQueue --> surrealDb

    %% ── Styles ────────────────────────────────────────────────────────
    style Sources fill:#daeef7,stroke:#7ab8d4
    style Ingestor fill:#daf7da,stroke:#7ad47a
    style CorePipeline fill:#f7f7da,stroke:#c8c87a
    style GraphEngine fill:#eedaf7,stroke:#b07ad4
    style CorePersist fill:#f7e4da,stroke:#d4987a
    style Detectors fill:#fff0da,stroke:#d4b07a
    style EnricherCrate fill:#f7daee,stroke:#d47ab0
    style ApiCrate fill:#dae4f7,stroke:#7a98d4
    style CliCrate fill:#eef7ee,stroke:#7ab07a
    style ProbeCrate fill:#f7f0da,stroke:#c8a87a
    style WorkbenchCrates fill:#e4f7f7,stroke:#7ac8c8
    style surrealDb fill:#f5f5f5,stroke:#999
    style ollama fill:#fce8fc,stroke:#c87ac8
```

**13 crates in a Cargo workspace** (resolver `"2"`, MSRV 1.87+ by intent — no `rust-version` key enforces it; toolchain pinned to `1.96.0` in `rust-toolchain.toml`, which also installs the `wasm32-unknown-unknown` target the workbench frontend needs):

| Crate | Package | Role |
|-------|---------|------|
| `schema` | `urban-integrity-schema` | Shared node/edge types with serde — 11 node kinds, 12 edge types, `SCHEMA_DEF`, plus the `Verdict`/`Coverage`/`Finding` probe vocabulary. Dual-licensed `Apache-2.0 OR AGPL-3.0-or-later` |
| `core` | `urban-integrity-core` | Graph engine, CSR snapshots, WAL, persistence |
| `ingestor` | `urban-integrity-ingestor` | Companies House + Contracts Finder + Scrapling clients, backfill, projector |
| `stats` | `urban-integrity-stats` | **Empty stub.** Still the untouched `cargo new` template — `add(left, right)` and its `it_works` test, 14 lines. `api` and `cli` both declare a dependency on it; no source file in the workspace references it. Scoring lives in `detector/src/scoring/` |
| `detector` | `urban-integrity-detector` | Motif detection (`motif/phoenix.rs`, `cluster.rs`, `nepotism.rs`) + the `P_c` scoring engine (`scoring/formula.rs`, `imd.rs`, `registry.rs`) and alert cache |
| `enricher` | `urban-integrity-enricher` | OSINT enrichment engine — 14 adapters + land-flip detector, exact-identifier resolution, `identity_links` + `land_flip_candidates` tables |
| `api` | `urban-integrity-api` | tonic gRPC + async-graphql over Axum |
| `probe` | `urban-integrity-probe` | The 22-probe forensic battery (`run.rs` `build_battery`), the dependency graph and gated dispatch (`graph.rs`), the oracle's population-source partition (`population_source.rs`) and the `evidence-report.md` writer (`report/`). Extracted from `cli` so the workbench can run the battery in-process |
| `cli` | `urban-integrity-cli` | clap-based analyst CLI (binary `urban-integrity-cli`); `probe` and `probe-oracle` are thin callers into `urban-integrity-probe` |
| `workbench` | `urban-integrity-workbench` | Native half of the analyst workbench: replay-only boot, the Axum `/api/*` routes merged onto the `api` router, graph algorithms (`graph/algorithms/`, Louvain via `graphrs`), Arrow IPC population export, and the embedded Trunk `dist/` (rust-embed) |
| `workbench-dto` | `urban-integrity-workbench-dto` | Serde-only wire types shared by server and browser — its only dependency is `serde`, so it compiles to wasm32 |
| `workbench-web` | `urban-integrity-workbench-web` | Leptos 0.8 CSR frontend (`publish = false`), built to wasm32 with Trunk; depends only on `workbench-dto` plus the leptos/wasm-bindgen stack, enforced by `scripts/check-wasm-boundary.sh` |
| `test-support` | `urban-integrity-test-support` | Shared SurrealDB test harness — one throwaway namespace per DB-gated test, removed on drop — plus the synthetic planted fixture graph (`planted.rs`) seeded by `fixture seed --planted` |

> The Python `scrapling-sidecar/` (FastAPI on `localhost:8765`) is a **separate process**, not a Cargo workspace member. Start it with `scripts/sidecar-start.sh`. Ingestion degrades gracefully with a WARN log if it is down.

## Quick Start

**[docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) is the complete, ordered path from a fresh clone on a
clean machine** — OS packages, toolchain, build, SurrealDB, a first run, tests and troubleshooting. The
short version (Linux; macOS and WSL work too, see GETTING-STARTED step A4), needing **no API keys and no data**:

```bash
# 0. Prerequisites (Debian/Ubuntu; see GETTING-STARTED for Fedora/macOS and for what each is for)
sudo apt-get install -y build-essential pkg-config libssl-dev protobuf-compiler curl git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
curl -sSf https://install.surrealdb.com | sh -s -- --version v3.1.0-beta.2   # → ~/.surrealdb/surreal
cargo install --locked trunk --version 0.21.14     # the wasm bundler; `trunk --version` must not say pg-trunk

# 1. Clone; rust-toolchain.toml pins Rust 1.96.0 + the wasm32 target, installed by rustup
git clone https://github.com/PotentSoftware/urban-integrity.git && cd urban-integrity
rustup show

# 2. Build the browser frontend, then the workspace (one cargo command at a time)
scripts/build-workbench.sh
cargo build --workspace

# 3. Start SurrealDB (memory-capped systemd user service where available, RocksDB store under data/)
scripts/surreal-start.sh

# 4. Seed the synthetic planted fixture into its own analysis, project it and run the probes
scripts/toy-analysis.sh

# 5. Open the workbench on it — use the exact command toy-analysis.sh printed
cargo run -p urban-integrity-cli -- workbench --analysis <printed db_name> --open
```

A fresh clone has **no data**: the store, the deprivation index and every cache live under the gitignored
`data/`. The committed `config.toml` is a **fictional example** — an empty example database, commented-out
planning portals with made-up codes — and is all the toy analysis needs. For real data, copy it to the
gitignored `config.local.toml` (read *instead of* `config.toml` when present) and set your own analysis,
wards and portals there. Working with real data — your own analysis and config, Companies House and Land
Registry keys, the deprivation index, the planning sidecar, `serve` — is step A7 of GETTING-STARTED, and
**[How long will it take?](docs/GETTING-STARTED.md#how-long-will-it-take)** gives the time and machine a
first real ingest needs (well over 12 hours on the maintainer's laptop, dominated by rate-limited
Companies House calls). Figures quoted in this README come from the maintainers' populated instance.

> **One nation per analysis.** A startup jurisdiction preflight derives the nation from the configured ward
> codes (ONS GSS prefix: `E05` England, `W05` Wales, `S13` Scotland, `N08` Northern Ireland) and **refuses** a
> config whose deprivation index belongs to a different nation, or whose wards span two. It refuses rather
> than warns because the wrong nation's index loads cleanly and matches zero wards, and because HM Land
> Registry filters on `company_number` with no geography test — a Scottish analysis would receive real English
> property and land-flip gate-3 would score it as local corroboration.

> **Storage backend:** SurrealDB (pinned `=3.1.0-beta.2`) runs on the RocksDB backend (`rocksdb://data/surreal-rocks.db`), launched via `scripts/surreal-start.sh`. This replaced the previous `surrealkv://` store on 2026-06-17 — surrealkv held every touched key resident in RAM and OOM-killed SurrealDB during the CCOD ingest. The launcher runs SurrealDB as a transient, memory-capped `urban-surreal` systemd **user** service (under `user.slice`, hard `MemoryMax=12G` by default), so a DB OOM reaps SurrealDB alone rather than taking the editor or host down. **Always launch via `scripts/surreal-start.sh`, never `surreal start` directly** (a guard reroutes a bare `surreal start`). Stop it with `systemctl --user stop urban-surreal`; tail logs with `journalctl --user -u urban-surreal -f` (also `data/surreal.log`). The old surrealkv store was kept as a rollback backup until 2026-07-28, then deleted (5.6 GB) once RocksDB had been authoritative for six weeks.

See [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) for full setup instructions.

## CLI Commands

The `Commands` enum in `crates/cli/src/main.rs` defines **23 subcommands**. The package builds a binary
named **`urban-integrity-cli`**, and the examples below use that name. Run it as
`cargo run -p urban-integrity-cli -- <command>`, as `target/debug/urban-integrity-cli`, or put it on
your `PATH` with `cargo install --locked --path crates/cli` (build the workbench frontend first if you
want the installed binary to embed it — see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md)).

```
urban-integrity-cli report             Generate an integrity report (--ward, --min-score, --motif, --from, --to, --format, --output-file, --entity, --summarize, --scope, --limit)
urban-integrity-cli alerts             List, inspect and withdraw flagged motif alerts (subcommands: list, show, withdraw)
urban-integrity-cli anomalies          List and adjudicate data anomalies — inputs the pipeline refused to guess at (subcommands: list, show, review)
urban-integrity-cli entity             Query entity profile by name or CH number (--ch-number, --output, --deep); includes LPA presence table and OSINT links
urban-integrity-cli graph-path         Find the shortest BFS path between two entities in the HIN graph
urban-integrity-cli status             Show graph engine status and statistics
urban-integrity-cli serve              Start the graph engine (API server + ingestors + background enricher)
urban-integrity-cli verify             Pre-signature counterparty audit (--company-number N) — outputs risk verdict with exit codes for CI integration
urban-integrity-cli ingest-planning    Scrape planning portal data on demand (--ward WARD_CODE or --all)
urban-integrity-cli project            Reproject graph_events WAL into native SurrealDB tables (for Surrealist exploration)
urban-integrity-cli backfill           Enrich graph nodes — subcommands: ch, ward, planning, cessation, officers, psc-links
urban-integrity-cli export-graphml     Export the graph to a Gephi-compatible GraphML file (--output graph.graphml, --source events|native (default: events), --node-type, --edge-type, --drop-isolated, --reachable-from-wards)
urban-integrity-cli enrich             Run OSINT enrichment adapters or the land-flip detector (--source all|land-flip|<adapter-name>)
urban-integrity-cli links              Manage identity link proposals — subcommands: list, show, confirm, reject
urban-integrity-cli flips              Review pre-permission land-flip candidates — subcommands: list, show, confirm, dismiss
urban-integrity-cli analysis           Provision and inspect analyses — subcommands: create, list, remove (per-analysis SurrealDB databases)
urban-integrity-cli fixture            Seed the synthetic planted fixture graph into an analysis created with --fixture (subcommand: seed --planted [--reseed])
urban-integrity-cli probe              Run the 22-probe forensic battery against graph_events and write evidence-report.md (--output, --json, --full-battery); verdict-gated by default
urban-integrity-cli probe-oracle       Compare every probe's declared population against an independent graph_events count, and state precisely why the rest cannot be checked (--output PATH)
urban-integrity-cli workbench          Boot a read-only replay snapshot and serve the analyst workbench UI (--bind ADDR, --open)
urban-integrity-cli migrate            Apply IN/OUT schema constraints to all 12 relation tables and re-project (run enrich first)
urban-integrity-cli purge-orphans      Remove orphan Property nodes + identity_links left by the pre-fix CCOD ingest (dry-run by default; --apply requires a typed digest)
urban-integrity-cli purge-ocod-owners  Retire superseded GB-OCOD: owner nodes + OwnsProperty edges from graph_events before an OCOD re-run (dry-run by default; --apply requires a typed digest)
```

Every command accepts a **global `--analysis <db_name>` flag** to target a named analysis database (bridged to the `URBAN__ANALYSIS` env var). Without it, the database from `config.toml` is used.

### report

> Reports, alerts and probe verdicts are **leads for qualified review**, not findings of wrongdoing.
> Read [DISCLAIMER.md](DISCLAIMER.md) before sharing any output.

```bash
# Ward-scoped table report (default output)
urban-integrity-cli report --ward X05999001 --min-score 0.7

# Markdown file export
urban-integrity-cli report --ward X05999001 --format md --output-file report.md

# PDF export
urban-integrity-cli report --ward X05999001 --format pdf --output-file report.pdf

# Entity-scoped report across all LPAs
urban-integrity-cli report --entity "Apex Build Ltd"

# Entity report with LPA presence breakdown
urban-integrity-cli report --entity "Apex Build Ltd" --scope all-lpa

# With LLM narrative summary (requires Ollama + gemma4)
urban-integrity-cli report --ward X05999001 --summarize
```

**`report` flags:** `--ward WARD_CODE`, `--min-score` (default 0.5), `--motif`, `--from`, `--to`, `--format md|pdf` (alias: `--output`, default: table), `--output-file PATH`, `--entity NAME`, `--summarize`, `--scope all-lpa`, `--limit` (default 100; 0 = all)

Withdrawn alerts are excluded from reports. Like `alerts list`, a truncated report prints `showing N of M matching alerts` rather than leaving the omission to be inferred.

### alerts list

```bash
# Recent alerts (last 24 hours)
urban-integrity-cli alerts list --since 24h

# Last 7 days, Phoenix motifs only, above 0.8 score
urban-integrity-cli alerts list --since 7d --motif phoenix --min-score 0.8

# Filter by ward, date range, cap results
urban-integrity-cli alerts list --ward X05999001 --from 2026-01-01 --to 2026-05-01 --limit 50

# All alerts, no truncation (warns on large datasets)
urban-integrity-cli alerts list --limit 0 --output json

# Include alerts an operator has withdrawn (hidden by default)
urban-integrity-cli alerts list --motif phoenix --include-withdrawn
```

**`alerts list` flags:** `--since Nh|Nd`, `--min-score` (default 0.5), `--ward`, `--motif`, `--from`, `--to`, `--limit` (default 100; 0 = unlimited), `--include-withdrawn`, `--output table|json`

Note: `--since` conflicts with `--from`/`--to` — use one or the other.

**Ordering is by score descending**, with `detected_at` then id as tie-breaks, so the order is total and the same query over the same data returns the same sequence every time. Filters are applied over the **whole** `MotifDetected` corpus (scanned through the paginated `scan_events_by_kind`), not inside a recency window, and a truncated page prints `showing N of M matching alerts, highest score first`. All three of those were defects until 2026-08-18, and between them they hid the only live Phoenix alert this project has produced. Filters cannot be pushed into SurrealQL because the payload lives in `result_json`, a JSON *string* SurrealDB cannot index into — so filtering happens in Rust, over a complete scan.

### alerts show

```bash
# Table output with four-block rendering (header, entities, description, score breakdown)
urban-integrity-cli alerts show <alert-id>

# JSON output with description_resolved (director slugs replaced by display names)
urban-integrity-cli alerts show <alert-id> --output json
```

### alerts withdraw

Detection upserts what it matches and never retracts what it *stops* matching, so an alert invalidated by a later data correction would otherwise sit in the queue indefinitely, indistinguishable from a live finding. `alerts withdraw` **marks** an alert rather than deleting it — a retracted false positive is evidence about detector quality, and this project has already destroyed that history once.

```bash
# Withdraw an alert a data correction has invalidated (reason is mandatory and stored)
urban-integrity-cli alerts withdraw <alert-id> --reason "rests on a name-only officer merge; split 2026-08-11"
```

`MotifResult` carries `withdrawn_at` and `withdrawn_reason` (both `serde(default)`, so alerts predating the fields still parse as live). Withdrawn alerts are hidden by default on every publishing surface — `alerts list`, `report`, GraphQL and gRPC — and readable via `alerts list --include-withdrawn`. Re-withdrawing an already-withdrawn alert is refused rather than silently overwriting the original reason and date. **A withdrawal is not a permanent suppression:** if the motif matches again, the upsert overwrites `result_json` and clears the withdrawal — correct, because it is then a live finding again.

### anomalies

An anomaly is **not** an alert. An alert says *"this looks like corruption"*; an anomaly says *"this input
could not be trusted, and here is exactly what was skipped"*. They are kept in separate tables so
data-quality noise never enters an analyst's corruption queue, and so a genuine finding cannot hide among
parse failures.

```bash
# Open anomalies, highest severity first
urban-integrity-cli anomalies list

# One kind only, or include already-adjudicated rows
urban-integrity-cli anomalies list --kind officer_key_collision
urban-integrity-cli anomalies list --all

# Full evidence for one anomaly (pretty-prints the nested evidence_json)
urban-integrity-cli anomalies show <anomaly-id>

# Adjudicate: accept it, or record it as not worth acting on
urban-integrity-cli anomalies review <anomaly-id>
urban-integrity-cli anomalies review <anomaly-id> --dismiss
```

Rows live in the `data_anomalies` table (`kind`, `severity`, `subject`, `detail`, `evidence_json`, `source`,
`detected_at`, `status`). Severity ordering is done in Rust, not SurrealQL — `severity` is a string column, so
a lexical `ORDER BY` would sort `high` after `low`. Anomaly keys legitimately contain colons (they are built
from the source and the subject, e.g. `officer_key_collision_backfill:officers:audit_director:…`), so only the
`data_anomalies:` table prefix and SurrealDB's backtick quoting are stripped on lookup.

> **Known gap:** `data_anomalies` has no retraction path. An audit that stops reporting a finding leaves the
> old `open` row in place indefinitely — the same shape as the alert problem `alerts withdraw` exists to solve.

### entity

```bash
# Look up by name (table output, default); includes LPA presence breakdown
urban-integrity-cli entity "Apex Build Ltd"

# Look up by CH number
urban-integrity-cli entity --ch-number 12345678

# JSON output for piping to jq (includes lpaPresence array)
urban-integrity-cli entity "Apex Build Ltd" --output json | jq '.directors[].name'

# Use a director slug seen on another profile
urban-integrity-cli entity apex-build-ltd

# Show sub-candidate OSINT links (0.50–0.69 confidence, investigative only)
urban-integrity-cli entity "Apex Build Ltd" --deep
```

The `entity` command renders an **LPA Presence** table showing how many contracts and planning applications an entity has per Local Planning Authority. The JSON output includes a `lpaPresence` array with `lpaCode`, `contractCount`, and `planningApplicationCount` fields.

The `--deep` flag adds a "Sub-Candidate OSINT Links (0.50–0.69, investigative only)" section sourced from the `identity_links` table. The sub-candidate-band links come from the **external yente OpenSanctions service** (which caps name-only matches at 0.65); UrbanIntegrity does not generate sub-candidate links from in-process fuzzy matching. Sub-candidate links cannot be confirmed or rejected via the `links` CLI — they are for investigative reference only.

### graph-path

```bash
# Trace the shortest path between a company and a councillor
urban-integrity-cli graph-path "Apex Build Ltd" "John Smith"

# Use slugs or CH numbers for exact disambiguation
urban-integrity-cli graph-path GB-COH:12345678 director:john-smith

# JSON output (nodes + edges arrays)
urban-integrity-cli graph-path "Apex Build Ltd" "John Smith" --output json
```

`graph-path` performs a BFS traversal over the HIN graph (up to 6 hops) loading edges from `graph_events` in SurrealDB. Output format: `Entity A → EDGE_TYPE → Entity B → EDGE_TYPE → Entity C`. Edge types render in SCREAMING_SNAKE_CASE (e.g. `OWNS_PROPERTY`, `AWARDED_TO`, `DISQUALIFIED_FROM`). Ambiguous names prompt a slug/CH-number disambiguation list.

### verify

Pre-signature counterparty audit — reads existing SurrealDB records and returns a risk verdict with machine-readable exit codes suitable for CI integration.

```bash
# Audit a counterparty before contract signature
urban-integrity-cli verify --company-number 12345678

# Exit codes: 0=low risk, 1=critical/high risk (evasion/disqualification detected)
urban-integrity-cli verify --company-number 12345678 && echo "Safe to proceed"
```

The verdict includes: status, P_c percentile, motif flags, dissolved-entity count, RISK_LEVEL, and OSINT flags — `has_disqualified_director`, `beneficial_owners`. A matched disqualified-director evasion pattern forces CRITICAL risk and non-zero exit code regardless of P_c score.

**`verify` flags:** `--company-number N`, `--output table|json`, `--summarize` (Ollama narrative, degrades gracefully if unavailable)

### project

Projects the `graph_events` WAL into native SurrealDB tables, making the graph browsable in tools like [Surrealist](https://surrealist.app).

The command wipes and fully reprojects these **11 node tables**: `entity`, `officer`, `location`, `contract`, `ward`, `political_actor`, plus the OSINT node tables `psc`, `charity`, `property`, `offshore_entity`, `disqualification` — and these **12 relation tables**: `directs`, `located_at`, `awarded_to`, `issued_by`, `represents`, plus the OSINT relation tables `controls`, `trustee_of`, `owns_property`, `offshore_link`, `donates_to`, `disqualified_from`, `declares_interest`.

Three of those tables are newly modelled or newly re-pointed, and hold **no rows in any live analysis yet**. A CDDA disqualification is now
its own node kind (`NodeWeight::Disqualification`) with `disqualified_from` redefined `FROM officer TO
disqualification`: the edge previously pointed at `disq:{resource_id}`, an id no `AddNode` ever created, so
the projected table was permanently empty. A ban bars a person from directing *any* company, and Companies
House names none, so pointing the edge at a company would assert something the source never states. A
councillor's register-of-interests declaration likewise gets its own `declares_interest` relation rather than
reusing `Directs` — "is a director of" is a Companies House fact and "declared an interest in" is a
self-reported disclosure, and `charity_crossover` scans `Directs` with no source-kind filter, so a
declaration arriving as a `Directs` edge would be read as a directorship inside a conflict-of-interest
finding. Both need a fresh `enrich` pass (`ch_disqualified`, `council_registers`) *and then* `project`
before they carry data — the WAL has nothing to project yet. Measured 2026-08-25 on `an_anglesey_aebaa785`:
`disqualification`, `disqualified_from` and `declares_interest` are 0 projected rows, and `graph_events`
holds 0 matching events.

```bash
# Reproject the full graph (idempotent — safe to run multiple times)
urban-integrity-cli project
```

The projector folds **`NodeUpdated` alongside `NodeAdded`**, so status corrections (dissolutions, liquidations) written after a node first appeared do reach the projected tables — for most of this project's life they did not, and the projected tables silently disagreed with the hot graph. It also parses `SCHEMA_DEF` and fails on any schema-defined field the projection would drop, surfaces edge-drop loss explicitly, and fails loud when the skip ratio is catastrophic (>90%) rather than emitting a partial graph. `--dry-run` is accepted as a flag but is not yet implemented — it prints a message and exits without writing.

### backfill

Enriches existing graph nodes with data not captured during initial ingestion. Six sub-modes:

```bash
# Enrich planning applicant nodes with Companies House numbers (requires CH API key)
urban-integrity-cli backfill ch

# Dry-run to see what would change without writing
urban-integrity-cli backfill ch --dry-run

# CH REST API rate (default: 2 req/s — CH allows 600 requests / 5 minutes and throttles above it)
urban-integrity-cli backfill ch --rate-limit 2

# Discard the saved resume cursor and walk every candidate again
urban-integrity-cli backfill ch --restart

# Resolve ward codes for Contracts Finder contracts via postcodes.io
urban-integrity-cli backfill ward

# Re-scrape the wards still attributed to the unknown-applicant placeholder,
# and stamp explicit applicant provenance across the planning corpus
urban-integrity-cli backfill planning

# Fetch real `date_of_cessation` from Companies House, so Phoenix's window is anchored where the
# company actually failed rather than where the graph first observed it
urban-integrity-cli backfill cessation --dry-run
urban-integrity-cli backfill cessation --rate-limit 2 --max-fetches 500

# Fetch officers for CH entities that have none, keyed on the identity evidence CH publishes
# (officer id, date of birth) rather than on the name alone
urban-integrity-cli backfill officers --dry-run

# Audit the officer keys ALREADY in the graph and record an `officer_key_collision` anomaly for
# each key that conflates people Companies House says are different
urban-integrity-cli backfill officers --audit

# Move officer events off the uncorroborated colliding keys the audit reports. Rewrites
# graph_events — a data correction, so dry-run first and `project` afterwards
urban-integrity-cli backfill officers --split-collisions --dry-run

# Re-run the PSC↔officer identity join over data already in the graph — no CH API calls
urban-integrity-cli backfill psc-links
urban-integrity-cli backfill psc-links --dry-run
```

`backfill ch`, `cessation` and `officers` all require `URBAN__ENRICHER__CH__API_KEY` — the REST key, not the
streaming key `serve` uses; the streaming key 401s against the REST search endpoint. All three default to
`--rate-limit 2` because Companies House allows 600 requests per 5 minutes and throttles above it (a run at
5/s produced `CH API 429` errors within the first minute). A full `backfill ch` pass is ~20,000 lookups at
2 req/s (several hours), so an interrupted run resumes from a saved cursor by default; `--restart` discards
it. `backfill officers` supersedes `scripts/ch_backfill.py --officers-for-ch-entities` as a first-class,
resumable subcommand — officers are the only path by which officer nodes reach the graph, since the CH
company-profile stream never emits appointments. `backfill psc-links` shares the `ch_psc` adapter's own matcher, gate and record builder, so the two paths cannot diverge, and is idempotent (existing `ch_psc` links are skipped, not duplicated). Run `backfill` before `project` so the native tables carry complete CH numbers and ward codes.

### enrich

Runs OSINT enrichment adapters via the `enricher` crate, or — with `--source land-flip` — the post-adapter Pre-Permission Land Flip detector. Adapters scan the graph for cross-registry entity matches and write **exact-identifier** proposals to the `identity_links` SurrealDB table for analyst review. All links originate from exact joins (exact CH `company_number` or exact normalised-name equality); the only externally-scored source is OpenSanctions, which queries a self-hosted yente service.

Available adapters (14):

| Adapter name | Source | What it does |
|-------------|--------|--------------|
| `ch_psc` | Companies House PSC API | Beneficial ownership (Persons with Significant Control) |
| `ch_disqualified` | Companies House Disqualified Directors | Banned directors directing active companies → Evasion alerts |
| `land_registry_ccod` | HMLR CCOD bulk CSV | UK-company property ownership nodes + `OwnsProperty` edges; requires `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` |
| `land_registry_ocod` | HMLR OCOD bulk CSV | Overseas-company property ownership nodes + `OwnsProperty` edges, keyed by canonical owner id (see below); bounded OCOD↔ICIJ name match hard-capped at 0.65; shares the HMLR account key with CCOD |
| `london_gazette` | London Gazette REST API | Corporate insolvency signals (InLiquidation) from winding-up + liquidator notices |
| `charity_commission` | Charity Commission publicextract.* | Exact company-number join for charitable entities in procurement |
| `opensanctions` | OpenSanctions / external yente | PEP and sanctions screening; requires `non_commercial_confirmed=true` in config |
| `icij` | ICIJ Offshore Leaks | Offshore entity structures in procurement (exact-identifier join; batched, fail-loud writes) |
| `electoral_commission` | Electoral Commission donation CSV | Political donation → contract correlation via exact CH company number |
| `council_registers` | Council Registers of Interest (Scrapling) | Councillor declarations via exact normalised-name match → nepotism bridge inputs |
| `opencorporates` | OpenCorporates API | Cross-jurisdiction company lookup via exact CH number; requires `oc_open_data_confirmed=true` + API key |
| `ppd` | HMLR Price Paid Data (OGL v3 open data, no key) | Staging-only adapter — downloads yearly PPD files, filters to the configured LPA postcode districts, UPSERTs into `ppd_transactions`; never emits graph mutations. Feeds the land-flip detector |
| `mutuals_register` | FCA Mutuals Public Register (bulk CSV, no key) | Ingests **registered societies** — co-operatives, community benefit societies, housing associations — as `GB-MUT:{registration}` Entity nodes with their registered address as a `Location` + `LocatedAt` edge. Creates no identity link of its own |
| `planning_identity` | Internal graph join (no credential) | Links `director:{slug}` planning-applicant nodes to CH-sourced Entity/Officer nodes on exact `normalise_name` equality; writes both an `identity_links` audit row and a real `Directs` edge on auto-link |

```bash
# Run all enabled adapters (disabled adapters log WARN and skip)
urban-integrity-cli enrich --source all

# Run a specific adapter by name
urban-integrity-cli enrich --source ch_psc
urban-integrity-cli enrich --source london_gazette
urban-integrity-cli enrich --source land_registry_ccod
urban-integrity-cli enrich --source land_registry_ocod
urban-integrity-cli enrich --source ppd
urban-integrity-cli enrich --source mutuals_register
urban-integrity-cli enrich --source planning_identity

# Run the Pre-Permission Land Flip detector (post-adapter; writes the review queue)
urban-integrity-cli enrich --source land-flip
```

Output reports candidates found, auto-linked records (above the confidence auto-link threshold), and records pending analyst review. Use `urban-integrity-cli links list` to inspect pending proposals, and `urban-integrity-cli flips list` to inspect land-flip candidates.

**Keyless adapters** (no API key required): `london_gazette`, `charity_commission`, `icij`, `electoral_commission`, `council_registers`, `ppd`, `mutuals_register`, `planning_identity`.

**Canonical OCOD owner keys.** OCOD owners used to be keyed `GB-OCOD:{country}-{id}`, so a single overseas
company fragmented into one node per way the source data spelled its country — EAUBRINK FARM GMBH arrived as
both `"Germany"` and `"Hamburg"` (a city, not a country) on the same `FC025391`. `owner_key`
(`crates/enricher/src/adapters/land_registry_ocod.rs`) now keys an owner on a genuine company number alone
(`GB-OCOD:fc025391`), because the number identifies the company and the jurisdiction term adds nothing but
fragmentation. Two narrow exceptions keep the country-keyed form, because merging on their number would fuse
unrelated companies into one apparent property accumulator — the exact corruption-shaped artifact the
detectors exist to find:

- **Sentinel numbers**, detected structurally rather than by list: strip any leading alphabetic prefix, and a
  remainder that is empty or all one repeated digit is a placeholder. `FC000000` is the live case (five
  unrelated companies across five jurisdictions); `FC111111` or `00000000` would be caught the same way.
- **`FC008194`**, the one structurally-genuine number a live measurement proved carries two unrelated
  companies (ROBIN SWINDON LTD, British Virgin Islands; CREDIT AGRICOLE CORPORATE AND INVESTMENT BANK,
  France). It is an audited single-entry exception in `NON_UNIQUE_COMPANY_NUMBERS`, matched
  case-insensitively; adding to that list requires the same live evidence.

`GB-ROE:OE######` keys (UK-issued overseas-entity ids) are unaffected. Because `graph_events` is append-only,
the key change only takes effect after `purge-ocod-owners --apply` retires the superseded owner events — see
that command below.

**Why registered societies matter.** A registered society is a corporate body that is *not* a company: constituted under the Co-operative and Community Benefit Societies Act 2014, registered with the FCA, and carrying a number with a letter suffix (`22360R`) unrelated to any Companies House number. Companies House ingest cannot see them, and a society with charitable rules is an **exempt charity** absent from the Charity Commission register too — two independent blind spots over the same organisations. Housing associations are among the largest recipients of public land and money in Wales; measured live on 2026-08-11, ClwydAlyn Housing held 52 Anglesey planning applications and reached the graph as a bare applicant string.

**Graph mutation in CLI mode.** The CLI `enrich` command wires the graph-mutation channel into a drain task that writes OSINT-sourced graph nodes and edges (PSC nodes, offshore nodes, property nodes, society nodes) directly to `graph_events`. The drain session is re-authenticated hourly against the resolved analysis database, so long-running adapter passes survive SurrealDB's session-token expiry. If `serve` is running concurrently, its in-memory hot graph stays stale until the next `project` run — this is accepted and expected. The land-flip detector (`--source land-flip`) and the PPD staging adapter emit **no** graph mutations by design.

The **Property Accumulation Anomaly detector** runs automatically after the CCOD adapter and fires a HIGH alert (`risk_score = 0.75`) when an Active entity owns more than 3 properties whose total `price_paid` exceeds 5× the entity's total awarded contract value (requires at least 1 awarded contract).

The **Donation↔Contract Correlation detector** fires within a 730-day bidirectional window when a political donor entity has been awarded contracts by an authority controlled by their recipient party. Requires Electoral Commission donation data (`enrich --source electoral_commission`) and the `data/party_authority_control.json` party map.

The **Pre-Permission Land Flip detector** runs via `enrich --source land-flip` after PPD staging. For each in-scope planning application it requires **all three** gates to pass before emitting a candidate:

1. **gate-1 (structural address)** — normalised postcode AND PAON AND street must all match a staged PPD transaction (a postcode-only match yields zero candidates).
2. **gate-2 (temporal window)** — the transfer falls in `TIER_1` (inside the open application window, bounded above at 365 days), `TIER_2` (0–12 months before) or `TIER_3` (12–24 months before); >24 months or no anchor date is rejected.
3. **gate-3 (connection corroboration)** — a ≥0.70 `auto_linked`/`confirmed` identity link from the applicant's net (applicant + directors + PSCs) to the matched `property:*` node.

A run prints where every application died — gate-1 no PPD match (and how many matched on postcode alone), gate-3 no corroboration, gate-2 no anchor date, gate-2 outside window, and candidates produced — because a bare "0 candidates" reads identically for four different problems with four different fixes. A failed gate-3 query is reported as a WARNING, since incomplete is not the same as negative.

Passing candidates are written to `land_flip_candidates` with `buyer_identity_confirmed: false` and a **MEDIUM severity ceiling** (there is no HIGH variant — a structural, compile-time-enforced safety invariant). No buyer is ever named in the hot graph. ≥3 sequential parcel purchases at a postcode/street cluster downgrade the candidate to LOW with a stored `suppression_reason`. Review candidates with `urban-integrity-cli flips`.

### links

Manages identity link proposals in the `identity_links` table. Links are produced by `enrich` and require analyst confirmation before they are injected into the hot graph.

```bash
# List every pending_review link, highest confidence first
urban-integrity-cli links list

# Show full evidence detail for a specific link
urban-integrity-cli links show <link-id>

# Confirm a link (updates status to confirmed)
urban-integrity-cli links confirm <link-id>

# Reject a link and suppress re-proposal
urban-integrity-cli links reject <link-id>
```

Sub-candidate links (confidence 0.50–0.69) are not shown in `links list` — use `entity --deep` to inspect them. Only `pending_review` and above links are actionable via `links confirm`/`reject`.

`links list` reads the **complete** `pending_review` set through the paginated `persistence::scan::scan_identity_links` and sorts by confidence in Rust, for display only. Until 2026-08-23 it issued an `ORDER BY confidence DESC LIMIT 100`, which is the worst shape a review queue can have: the truncation is silent, and it always drops the *lowest*-confidence links — exactly the ones an analyst is being asked to adjudicate. A DB-gated regression seeds 150 rows and asserts the lowest-confidence marker row now surfaces.

### flips

Reviews Pre-Permission Land Flip candidates in the `land_flip_candidates` table. Candidates are produced by `enrich --source land-flip`. Confirming or dismissing a candidate is a **review-queue action only** — it never writes to `graph_events`/the hot graph (a confirmed land-flip is an investigative artifact, not a graph edge).

```bash
# List every pending candidate (newest first — the whole queue, not a 100-row page)
urban-integrity-cli flips list

# Show all three gate-evidence sections for one candidate
urban-integrity-cli flips show <candidate-id>

# Confirm a candidate as a genuine land-flip (optional analyst note)
urban-integrity-cli flips confirm <candidate-id> --note "verified against planning portal"

# Dismiss a candidate and suppress re-proposal (optional analyst note)
urban-integrity-cli flips dismiss <candidate-id> --note "address mismatch on inspection"
```

The free-text `--note` is bound (never interpolated) and is never logged at INFO — only the candidate id and status are logged.

Like `links list`, `flips list` reads the whole `pending_review` set through `scan_land_flip_candidates` and orders newest-first in Rust; its former `ORDER BY created_at DESC LIMIT 100` silently hid the oldest candidates once the queue passed 100.

### analysis

Provisions and manages **named analyses** — isolated per-analysis SurrealDB databases (named `an_<label-slug>_<wardset-hash8>`) tracked in a `_catalog` registry table. Every other command targets an analysis via the global `--analysis <db_name>` flag (bridged to `URBAN__ANALYSIS`); without it, the `config.toml` database is used.

```bash
# Provision a new analysis from a ward-code file (one ONS GSS code per line)
urban-integrity-cli analysis create --label "Black Country 2026" --wards-file wards.txt

# Clone an existing baseline database instead of starting empty
urban-integrity-cli analysis create --label "Example District" --wards-file example-wards.txt --from uk_national

# List provisioned analyses (newest first)
urban-integrity-cli analysis list

# Dry-run removal report (default), then actually drop
urban-integrity-cli analysis remove an_foo_deadbeef
urban-integrity-cli analysis remove an_foo_deadbeef --apply

# Removing an analysis still inside the Phoenix detection window requires explicit intent
urban-integrity-cli analysis remove an_foo_deadbeef --apply --decommission
```

**`analysis create` flags:** `--label` (human label, slugified into the database name), `--wards-file` (file of ONS GSS ward codes, one per line), `--from BASELINE` (whole-DB clone via `surreal export | import` — one-directional and read-only on the source), `--boundary-vintage` (ONS boundary edition recorded in the registry; default `unknown`), `--fixture` (mark the analysis as a fixture target — `fixture seed` refuses any analysis created without it).

`analysis remove` is **dry-run by default**; `--apply` drops the database, deletes its checkpoint directory, and marks the registry row `retired`. Analyses still inside the Phoenix detection window (measured from the registry `created_at`; detector default 180 days) are refused unless `--decommission` explicitly retires live evidence.

### purge-orphans / purge-ocod-owners

Two destructive data-maintenance commands. Both are **dry-run by default**, both share one confirmation and
delete implementation in `crates/cli/src/commands/purge.rs`, and neither has a `--yes`, `--force` or
environment-variable escape.

```bash
# Count orphan Property nodes + land_registry identity_links (reads only, deletes nothing)
urban-integrity-cli purge-orphans

# Count superseded GB-OCOD: owner nodes and their OwnsProperty edges
urban-integrity-cli purge-ocod-owners

# Delete — prints a digest derived from THIS scan's id list and waits for you to type it back
urban-integrity-cli purge-orphans --apply
urban-integrity-cli purge-ocod-owners --apply
```

**The `--apply` handshake.** The scan is the read-only first pass; `--apply` sorts and de-duplicates the ids
it produced, prints their SHA-256 digest, and deletes exactly that in-memory list — never a post-confirmation
re-scan, so what you confirm is what is deleted. `confirm_interactively` **hard-errors when stdin is not a
terminal** rather than returning "not confirmed", so a purge cannot be run from a pipe, a cron job or a CI
step by accident.

**Deletion is counted, not assumed.** `delete_in_batches` issues
`DELETE type::table($table) WHERE id IN $ids RETURN BEFORE` in 10,000-key batches and reports
`removed.len()` — the rows SurrealDB actually removed — never the number of keys sent. Any mismatch is a hard
error. That check is load-bearing: the first live `purge-ocod-owners` run on 2026-08-22 reported
`sent 10000 keys, SurrealDB removed 0` and aborted, because `record_key` was leaving SurrealDB's backtick
quoting on the record key so every delete addressed a key that matched no row. `purge-orphans` shares the
helper, so its `--apply` path was equally broken and its 0-orphan live run could not have revealed it. Both
are fixed; this is the same failure class as the ICIJ adapter logging 814k rows written against an empty
table, applied to a delete.

**`purge-orphans` also carries a 1% blast-radius cap.** An orphan is a `property:` `NodeAdded` row that is
not the destination of any `OwnsProperty` edge. If the orphan share of the scanned property population
exceeds 1%, `--apply` refuses with an error instead of deleting. The pre-existing guard only refused when the
owned set was *empty*, which would not have caught the historical misclassification that flagged 93,052
legitimate OCOD records as orphans — that owned set was non-empty, just built from the wrong definition.

**`purge-ocod-owners` has no ratio cap, deliberately.** Its correct scope is 100% of the `GB-OCOD:`-sourced
owner rows, so a cap would fire on the success case. `graph_events` is append-only and the OCOD adapter has
no incremental skip guard, so a re-run re-emits every owner: retiring the superseded events first is what
makes the corrected owner key *merge* nodes instead of adding correctly-keyed ones beside the fragmented
ones. `GB-ROE:` owners are never in scope, and Property `NodeAdded` rows are left alone (the projector's
`UPSERT` is idempotent on an unchanged property id). Follow an `--apply` with
`enrich --source land_registry_ocod` and `project`. Live run 2026-08-22 against `an_anglesey_aebaa785`:
127,407 rows retired (29,944 `OffshoreEntity` nodes + 97,463 `OwnsProperty` edges, exported to JSONL first),
re-emitted and re-projected; `owns_property` unchanged at 105,810 and `offshore_entity` 29,944 → 29,927.

### probe

Runs the **forensic probe battery** against `graph_events` — the Rust port of
`scripts/analyst_probe.py`. All **22** of that script's probes are registered in `build_battery`
(`crates/probe/src/run.rs`), in the script's exact `OSINT_PROBES` order: 7 Tier-1,
12 Tier-2 and 3 Cluster-A anchor probes. The command also **writes the report** — `evidence-report.md`
is assembled by `crates/probe/src/report/`, not by the Python script. The battery lives in its own
`urban-integrity-probe` crate so the workbench runs the identical code path in-process.

```bash
# Run the battery against the configured analysis; writes ./evidence-report.md
urban-integrity-cli probe

# Write the report somewhere else, and capture the raw 22-Finding battery as JSON
urban-integrity-cli probe --output /tmp/evidence.md --json /tmp/battery.json

# Run every probe, ungated, so a gated run can be diffed against the complete one
urban-integrity-cli probe --full-battery

# Target a named analysis with the global selector
urban-integrity-cli --analysis <db_name> probe
```

**`probe` flags:** `--output PATH` (report destination; default `evidence-report.md` in the working
directory), `--json PATH` (raw `Finding` battery, independent of `--output`), `--full-battery`.

**Dispatch is verdict-gated by default.** A probe declares its prerequisites through
`Probe::depends_on()`, and the orchestrator skips it when they did not fire: every declared dependency must
have returned `HIT` or `WEAK` (AND), and a Tier-2 probe additionally needs *at least one* Tier-1 probe to
have returned `HIT`/`WEAK` (OR). The graph is validated before anything runs — a dependency naming an
unregistered probe, a cycle, or a registration order interleaving the tiers is a hard error, never a silent
no-op. The shipped battery declares **no** dependency edges — the one it first shipped with,
`address-cluster-exact → identity-integrity`, was removed on 2026-09-03 because `identity-integrity` can
never return `HIT`/`WEAK`, so the gate could never open. All gating today comes from the Tier-1/Tier-2 rule. `--full-battery` opts out of gating (the graph is
still validated), and `probe-oracle` always runs ungated so its comparison stays complete.

The seven Tier-1 probes:

| # | Probe | What it measures |
|---|-------|------------------|
| 1 | `precomputed-alerts` | `MotifDetected` alerts already in the graph, counted whatever the `detector_runs:current` marker says |
| 2 | `identity-integrity` | Planning-applicant provenance coverage across `planning:` Contract nodes, compared against the `_catalog.analyses.coverage_baseline_json` regression baseline — the one probe needing a second connection |
| 3 | `detector-readiness` | Which motif detector is starved, and by which specific missing term |
| 4 | `address-cluster-exact` | Distinct applicants sharing one exact address |
| 5 | `geographic-concentration` | Repeated streets/sectors across `Location` nodes |
| 6 | `ward-concentration` | Contracts per ward via `Represents` edges |
| 7 | `temporal-burst` | Busiest ingestion day, always stated against the total population |

The remaining 15 (12 Tier-2 plus 3 Cluster-A anchors) read the OSINT layer: `identity-link-registry`,
`osint-evasion-alerts`, `osint-disqualified-directors`, `psc-beneficial-ownership`,
`gazette-insolvency-signals`, `property-nodes`, `phase20-ec-donations`, `phase20-council-registers`,
`osint-confirmed-intel`, `osint-analyst-queue`, `osint-new-node-types`, `osint-risk-edges`, and the anchors
`land-flip-candidates`, `donation-contract-correlation`, `opencorporates-cross-jurisdiction`.

Each returns a `Finding` carrying one of **seven** verdicts — `HIT`, `WEAK`, `CLEAR`, `NOT_APPLICABLE`,
`DATA_GAP`, `BLOCKED`, `SKIPPED` — rendered with the `population/capable` coverage pair the verdict was
drawn from (`SKIPPED` has no coverage: the probe never ran).
**The vocabulary is the point.** `Clear` is unconstructible except through `Verdict::classify`, which
returns `NotApplicable` (with a required reason) whenever the structurally-capable subset is smaller than
the population — so a probe can no longer report "nothing suspicious found" about a population it never
measured. That is enforced by the type system rather than by convention: `Coverage`'s fields are private
and its only constructor runs inside `classify`, so `Verdict::Clear(Coverage { population: 5, capable: 3 })`
in another module is a compile error (E0451), and `Verdict` derives `Serialize` only, so the same
invariant-violating value cannot be smuggled in from JSON either. Two concrete instances of the defect are
closed in this port: `geographic-concentration` returned Python's "geographically dispersed" `CLEAR` when
there were **zero** `Location` nodes, and
`precomputed-alerts` could not tell zero alerts from a detector that had never run. A probe that hits an
internal error returns a `DATA_GAP`/`BLOCKED` finding rather than aborting the battery.

`Verdict::Skipped` carries the same construction discipline. Its `SkipCause` wraps a module-private
inner type with two arms — `Dependency { upstream, upstream_verdict }` and
`TierAggregate { tier, probes_checked }` — reachable only through `Verdict::skipped_dependency` /
`skipped_tier_aggregate`, both of which derive the recorded discriminant from a real upstream `&Verdict`.
A skipped probe therefore cannot name a cause that did not actually gate it, and a skip can never be
mistaken for a `Clear`.

`detector_runs:current` is written by `serve`'s snapshot-bake path, and only when a detector is actually
configured — a bake with no detector leaves the table untouched rather than writing `motif_count: 0` as a
stand-in for "never ran". An analysis that has not run `serve` since that marker shipped therefore has no
row. That absence no longer suppresses the alert count: `precomputed-alerts` used to return early on a
missing marker, which conflated "the Tier-B detector never ran" with "there are no alerts", even though
five enricher post-adapter detectors emit `MotifDetected` without ever touching the marker. It now always
counts and classifies, and the "did Tier-B run" signal is reported separately by `detector-readiness`
(adjudication **ADJ-02**, [`docs/ADJUDICATIONS.md`](docs/ADJUDICATIONS.md)).

**The report is written by this command.** `evidence-report.md` is assembled in
`crates/probe/src/report/` in a fixed order — header, BLUF, verdict legend, **Skipped
Probes**, "How the Search Adapted", then the per-verdict detail groups (Confirmed Findings, Suggestive
Patterns, Data-Integrity Gaps, Not Applicable, Verified Negative Findings), the ledger, contradictions and
remediation. ("Confirmed Findings" means a probe's test matched the data — HIT verdicts — not that any
wrongdoing has been confirmed.) The Skipped section renders **even when empty** ("No probes were skipped in this run."), so a
shorter report is never mistaken for a cleaner one, and each Verified Negative entry states whether its
population was oracle-corroborated or only self-reported. `scripts/analyst_probe.py` still exists as the
differential reference; it has none of the verdict-vocabulary or provenance fixes.

### export-graphml

Exports the graph as a Gephi-compatible GraphML file. Two sources are supported via `--source`, and the
default is **`events`**:

- **`events`** (default) — reconstructs the graph directly from the `graph_events` WAL (NodeAdded +
  EdgeAdded events) over raw HTTP `/sql`. No projection step required.
- **`native`** — reads the projected native SurrealDB tables (`entity`, `officer`, `location`, `contract`,
  `ward`, `political_actor` and relation tables `directs`, `located_at`, `awarded_to`, `issued_by`,
  `represents`; the OSINT tables are not included). Requires `urban-integrity-cli project` to have been run first.

`native` was the default until 2026-08-25. It goes through the SurrealDB SDK, whose flatbuffers codec fails
to decode typed `RecordId` fields on large result sets — a 358k-node analysis errors with *Failed to
deserialize flatbuffers payload*. It is kept for small graphs and for exporting exactly what `project`
produced, but the default should be the path that works at real scale.

The command builds a `petgraph::graph::DiGraph` in memory then serialises it to GraphML.

```bash
# Export to the default filename (graph.graphml in the current directory), from the WAL
urban-integrity-cli export-graphml

# Write to a custom path
urban-integrity-cli export-graphml --output /tmp/example-2026-08.graphml

# Read the projected native tables instead
urban-integrity-cli export-graphml --source native

# Slice the export by type (repeatable, case-insensitive) and drop what is left unconnected
urban-integrity-cli export-graphml --node-type Officer --node-type Entity --edge-type Directs --drop-isolated

# Keep only the weakly-connected component reachable from the analysis's wards
urban-integrity-cli export-graphml --reachable-from-wards                       # every configured ward_code
urban-integrity-cli export-graphml --reachable-from-wards X05999001,X05999002   # named codes only

# Drop degree-zero nodes from an existing export instead
python3 scripts/prune_isolated_nodes.py graph.graphml -o graph-connected.graphml
```

`--reachable-from-wards` has three distinct states: **absent** (no reachability filter), **present with no
codes** (falls back to every `ward_code` in the configuration), and **present with codes** (those codes
only). Reachability is computed over the *undirected* view before any type filtering, so type filters cannot
silently collapse the reachable set; a code list that resolves to zero seeds is a hard error, not an empty
file.

A `--node-type`/`--edge-type` value that matches nothing present is a **hard error listing what is actually
there**, never a silent empty export — the same failure class as an adapter reporting rows written against a
table it never wrote to. If the native tables are empty, `--source native` fails with a message directing you
to run `urban-integrity-cli project` first or to use `--source events`.

### serve

`serve` starts the full engine in a single process:

- **Startup replay** — existing `graph_events` are replayed (paginated) into the in-memory coordinator so detectors see historical data; if events exist and replay fails, `serve` refuses to start rather than run blind on an empty graph
- **Companies House streaming** — long-lived HTTPS connection; requires `URBAN__INGEST__CH_API_KEY`
- **Contracts Finder polling** — OCDS REST poller starts automatically
- **Planning portal polling** — activated when portals are configured in `config.toml`
- **Detection** — the only process that runs the motif detectors. Its `DetectorConfig` is read from the `[detector]` block of `config.toml` (`phoenix_window_days`, `phoenix_min_value`, `imd_csv_path`); an absent block falls back to a 180-day window, never wider
- **API server** — Axum HTTP on `:8080`, gRPC on `:8081`
- **Background enricher** — runs OSINT adapters on the configured interval; returns a `JoinHandle` cancelled on shutdown
- **Session keep-alive** — the SurrealDB session is re-authenticated every 50 minutes (belt-and-suspenders against the ~1-hour token expiry), with an inline retry on 401

```bash
export URBAN__INGEST__CH_API_KEY="your-key"
urban-integrity-cli serve
```

### probe-oracle

Every probe reports its own **population** — the size of the set it examined. Nothing used to check that
number, so a probe counting the wrong thing looked exactly like a probe counting the right thing.
`probe-oracle` closes that gap: it runs the real battery, then independently recomputes
each checkable population straight from `graph_events` and prints the two figures side by side.

```bash
# Compare declared vs independently recomputed populations
urban-integrity-cli probe-oracle

# Also write both tables to a file
urban-integrity-cli probe-oracle --output oracle-report.txt

# Against a named analysis
urban-integrity-cli --analysis <db_name> probe-oracle
```

The independent path is `urban_integrity_core::persistence::oracle` — `count_events`,
`count_distinct_entities` and `count_distinct_edge_field`, built on raw `count()` / `GROUP BY` SurrealQL.
It deliberately **shares no implementation** with `persistence::scan`, which is the entire point: two paths
that agree by construction cannot catch a defect in either. That independence is
enforced by `crates/core/tests/oracle_independence_guard.rs`, which fails the build if `oracle.rs` gains a
`scan::` import or call. Do not "helpfully" deduplicate the two.

Of the 22 registered probes, **10 are checked** and **12 are excluded**, each exclusion carrying a
category and a specific citation for where its declared population actually comes from — never a blanket
"Tier-2" label:

| Category | Meaning |
|----------|---------|
| `IdentityLinksSourced` | Population comes from `identity_links`, not `graph_events` |
| `VerdictThreshold` | Population is a hardcoded `Verdict::classify(1, 1, …)` threshold, not a real count |
| `SemanticMismatch` | Population counts something structurally different from an entity/edge count |
| `MarkerArithmetic` | Population is arithmetic on scalar fields of one marker record |

Every checked row is compared, with no by-name exemptions. When any row disagrees, the report prints a
pointer to [`docs/ADJUDICATIONS.md`](docs/ADJUDICATIONS.md) on stdout and in the `--output` artifact, so the
next disagreement is checked against the settled record before anyone re-investigates from zero.

**This surface has already earned its keep.** It caught defect **ADJ-01**: `geographic-concentration`
counted per `NodeAdded` *event* rather than per *entity*, so every re-ingested Location crossed the
"≥ 2 applications on a street" threshold on its own duplicates. Against `an_anglesey_aebaa785` there are
102,897 Location events but only 61,756 distinct Locations. Switching the probe to
`scan_latest_entities_by_type` took its `matches` figure from 23,322 to **1,697** — exactly the Python
reference figure, closing the whole 13.7× gap (commit `c8fb0c7`).

> **Two review findings are fixed (2026-09-01); one caveat survives them.** The module's claimed immunity to
> the SurrealDB index-planner bug was false for two of the three functions, which wrap a row-materialising
> inner `SELECT`. The claim is withdrawn, the query-shape guard's `count(` exemption is narrowed so both are
> visible, and each carries an `ALLOWLIST` entry stating the evidence (which is why `ALLOWLIST_BASELINE` went
> 6 → 8 — a recorded widening that made two previously *invisible* shapes visible, not a new exemption).
> **That evidence is a live cross-check — a sample, not a proof**, since the bug is run-to-run
> non-deterministic; re-run it for a new corpus or SurrealDB version. Separately, the coverage invariant was
> `debug_assert!`-only and compiled out of release; it is now a hard error checked in both directions against
> the battery's own length. Still open: `--output` can capture ANSI colour codes when stdout is a terminal, so
> the artifact is not guaranteed clean plain text.

### fixture

Seeds the **planted fixture** — a small synthetic graph (`crates/test-support/src/planted.rs`) whose probe
and algorithm answers are known in advance — into an analysis. It only writes into an analysis created with
`analysis create --fixture`, and it refuses to run without an explicit `--analysis`: it never targets the
default database.

```bash
urban-integrity-cli analysis create --label "Planted fixture" --wards-file wards.txt --fixture
urban-integrity-cli --analysis <slug> fixture seed --planted            # target graph_events must be empty
urban-integrity-cli --analysis <slug> fixture seed --planted --reseed   # wipe graph_events first
```

`scripts/toy-analysis.sh` does the whole chain idempotently — create (reading the ward code from
`planted.rs`), seed with `--reseed`, `project`, then `probe --output data/toy-evidence-report.md` — and prints
the `workbench --analysis <slug>` command to run next. Re-run it before any QA pass on the fixture, since a
stale fixture database reads as a less comprehensive fixture.

## Analyst Workbench

A browser UI for exploring one analysis, served by `urban-integrity-cli workbench`. The server half
(`crates/workbench`) replays the analysis's `graph_events` into an in-memory coordinator, computes the
**canonical population** — the weakly-connected component reachable from the analysis's own ward codes (its
`_catalog` row, falling back to `config.toml`'s wards), the same pipeline `export-graphml
--reachable-from-wards` uses — and serves a JSON/Arrow API merged onto the `api` crate's router. The browser
half (`crates/workbench-web`) is a Leptos 0.8 client-side app compiled to WebAssembly and embedded into the
binary with rust-embed.

```bash
# 1. Build the frontend once (Trunk 0.21.14 + the wasm32 target). Without this step the
#    binary serves a "workbench assets not built" stub page — cargo build never runs Trunk.
scripts/build-workbench.sh

# 2. Serve an analysis (default bind 127.0.0.1:8090 from config.toml [workbench].bind_addr)
cargo run -p urban-integrity-cli -- --analysis <db_name> workbench
cargo run -p urban-integrity-cli -- --analysis <db_name> workbench --bind 127.0.0.1:9000 --open
```

The listener is bound **before** any SurrealDB work, so a taken port fails in milliseconds naming the address
(no auto-picked port); the URL is printed as soon as the socket exists, and early browser requests queue
while the replay runs — which takes minutes on a live corpus. `--open` launches the default browser and is
off by default. Because `rust-embed` embeds `dist/` at compile time in every profile, rebuild the Rust binary
after re-running `scripts/build-workbench.sh`, or it keeps serving the previous frontend.

**Pages** (hash-routed): home, `analysis` (run the probe battery in-process and read the finding board;
download the same Markdown dossier the CLI `probe` writes via `/api/analysis/report.md`), `alerts` and
`alert/:id` (triage inbox with facet counts; withdraw with a stated reason), `graph` (Cosmograph canvas over
the canonical population, with canvas filters), `entity` and `entity/:id` (search, disambiguation, profile
and neighbourhood), `finding/:probe`, `investigations` and `investigations/:id` (saved investigations with
pinned entities), and `help` (glossary, algorithm explanations, CLI cheatsheet).

**Graph algorithms** run server-side through `POST /api/graph/algorithm` and are cancellable:
`all_simple_paths`, `shortest_paths`, `community` (seeded Louvain), `k_core`, `betweenness` and
`ward_subgraph`, plus `highlight_set` for resolving a deep link's entity set — which reports how many
requested ids are *not* in the population rather than silently dropping them. `GET /api/graph/population`
streams the population as Arrow IPC (`application/vnd.apache.arrow.stream`).

**Boundaries enforced in CI.** The `wasm-build` job builds `workbench-dto` and `workbench-web` for
`wasm32-unknown-unknown`, runs `scripts/check-wasm-boundary.sh` (no `schema`, `core`, `detector`,
`surrealdb`, `tokio`, `petgraph`, `rayon`, `graphrs` or `arrow` may enter the browser's dependency graph),
`scripts/check-fixture-not-shipped.sh` (the test-only graph assembler that fabricates a population must not
reach a shipped code path), an abbreviation-glossary guard, a Cosmograph shim drift check and a release Trunk
build.

The workbench is **replay-only**: it does not run ingestors or motif detectors. Its writes are analyst
actions — alert withdrawal, saved investigations and the stored latest probe report.

## Forensic Investigation Scripts

`scripts/` contains a Python evidence pipeline for manual forensic investigation against the live graph, plus shell tooling for local ops. The Python scripts are standalone and talk to SurrealDB directly over HTTP.

**Targeting an analysis.** The scripts no longer hardcode a `main` database — that name predates the analysis lifecycle and no CLI command reads it. `scripts/surreal_target.py` gives them the same selector the Rust CLI uses, with the same precedence:

```
--analysis <db_name>  >  $URBAN__ANALYSIS  >  $URBAN__SURREAL__DATABASE  >  config.toml [surreal].database
```

There is deliberately **no default**. SurrealDB creates a database on first write, so a script aimed at a name that does not exist would quietly populate a fresh empty one, find nothing, and report success. A preflight therefore refuses to run unless the target database already exists, and names the available alternatives when it does not. `analyst_probe.py` honours the same environment-variable precedence but exposes no `--analysis` flag — select its target with `URBAN__ANALYSIS`. `rebuild-anglesey.sh` takes `--analysis` directly.

| Script | Purpose |
|--------|---------|
| `pa_ingest.py` | Ingest elected councillors as `PoliticalActor` nodes from ModernGov |
| `ch_backfill.py` | Backfill Companies House data for known entities. Its `--officers-for-ch-entities` path is superseded by the first-class `backfill officers` subcommand, which is resumable and keys on CH identity evidence rather than the name alone |
| `ch_cessation_backfill.py` | Widen Phoenix's failed-company coverage by fetching real `date_of_cessation` from CH REST for entities the graph still calls Active (superseded by `backfill cessation`) |
| `ch_officer_coverage.py` | Close Phoenix's real gap — Contracts Finder awardee companies that carry no officers at all |
| `phoenix_funnel.py` | Measure where Phoenix candidates die, against `graph_events` rather than the projected tables (which hold a fraction of the log's `Directs` edges) |
| `build_deprivation_ward_index.py` | Build the ward-level deprivation index the detector joins to, for any one UK nation (`--nation england\|wales\|scotland\|northern-ireland`, plus `--lad <codes>` for england/wales/scotland, or `--all` for wales only; northern-ireland needs neither); emits the load-bearing `IMD Denominator` column |
| `projection_audit.py` | Classify each projected table as TRUNCATED, STALE, DROPPED or COMPLETE against `graph_events` |
| `cf_backfill_dates.py` | Backfill `date_published` onto Contracts Finder contracts already in the graph, from OCDS `release.date` (the mapper fix is forward-only — `graph_events` stores the mapped weight, not the raw OCDS) |
| `resolve_planning_applicants_to_ch.py` | Resolve free-text planning applicants against Companies House to give them a real identifier |
| `corroborate_offsite_applicant_matches.py` | Clear the applicant→company matches withheld for review, using a second corroborating signal rather than the name alone |
| `repair_psc_officer_links.py` | Repoint the `ch_psc` identity links whose officer target node does not exist |
| `rekey_officers_to_director_slug.py` | Re-key officer nodes from per-company `OFF:{company}-{NAME}` to the canonical `director:{slug}`, so one human is one node |
| `officer_merge_audit.py` | Adjudicate the name-only officer fusions that the re-key could not distinguish |
| `split_false_merged_officers.py` | Split back apart the officer nodes the name-only re-key wrongly fused |
| `normalize_officer_keys.py` | Normalise officer `EntityId` slugs to the cross-LPA dedup invariant |
| `fix_edge_directions.py` | Correct edge direction anomalies in the persisted graph |
| `pa_link_officers.py` | Link `PoliticalActor` nodes to CH officer records (unblocks NepotismBridge) |
| `land_flip_funnel.py` | Measure the land-flip funnel end to end against live data, using the shipped gate-1 rather than a copy of it |
| `land_flip_pairable.py` | Report which planning applications are even pairable with a staged PPD transaction |
| `prune_isolated_nodes.py` | Streaming two-pass removal of degree-zero nodes from a GraphML export |
| `analyst_probe.py` | Forensic probe battery — runs 22 probes and writes `evidence-report.md` (selects its target via `URBAN__ANALYSIS`, not a flag). **Superseded by `urban-integrity-cli probe`**, which carries all 22 probes plus the report writer; kept as the differential reference, and lacking every verdict-vocabulary, provenance and gating fix |
| `surreal_target.py` | Shared analysis-database resolver + existence preflight for the Python scripts — no default target, by design |
| `surreal-start.sh` | Launch SurrealDB (rocksdb backend) as an isolated, memory-capped `urban-surreal` systemd user service, or as a detached background process without systemd; `--stop` stops either |
| `surreal-guard.sh` | Interactive-shell guard that intercepts a bare `surreal start` and reroutes it through `surreal-start.sh` — prevents reintroducing the surrealkv OOM by reusing old shell history |
| `surreal-drop-test-ns.sh` | Sweep `test*` namespaces stranded by SIGKILLed test runs, where `NsGuard`'s `Drop` could not fire (dry-run by default; `--apply` to remove) |
| `sidecar-start.sh` | Launch the Scrapling planning sidecar (FastAPI/uvicorn on :8765) the same way every time |
| `check.sh` | One-shot local CI gate mirroring `.github/workflows/ci.yml` — runs fmt `--check`, clippy `-D warnings`, and workspace tests even if an earlier gate fails (`--fix`, `--db` flags) |
| `status.sh` | Read-only "where are we?" snapshot — recent commits, uncommitted changes, open findings, worklog tail |
| `rebuild-anglesey.sh` | Maintainer-specific: refresh the maintainers' reference analysis (one Welsh council) **without** re-fetching external data — prunes the derived layer (motif alerts + CCOD orphans) and regenerates it with the fixed detector |
| `wards` | Maintainer-specific: the reference analysis's 14 ONS GSS ward codes. Not an example to copy — scope your own analysis with your own `--wards-file` |
| `enrich.sh` | Convenience wrapper — starts SurrealDB if needed (via `surreal-start.sh`) and runs `urban-integrity-cli enrich --source all` |
| `report.sh` | Convenience wrapper — starts SurrealDB if needed (via `surreal-start.sh`) and runs the integrity report |
| `export_graphml.sh` | Convenience wrapper — runs `export-graphml --source events` (no `project` step required) |
| `export_parquet.sh` | Export the graph to timestamped GraphML + two Parquet files for Cosmograph. The ward-reachability filter it used to implement in Python now lives in `export-graphml --reachable-from-wards` |
| `build-workbench.sh` | The one place Trunk runs: builds `crates/workbench-web` (release) into `crates/workbench/dist/`. Refuses to run if the `trunk` on PATH is `pg-trunk` rather than the wasm bundler, or the wasm32 target is missing |
| `toy-analysis.sh` | Provision, seed, project and probe the planted fixture analysis (idempotent), then print the `workbench` command to open it |
| `check-wasm-boundary.sh` | Fail if a forbidden native crate enters the wasm32 dependency graph of a workbench-facing crate |
| `check-fixture-not-shipped.sh` | Fail if the test-only planted-graph assembler is referenced from a shipped code path |

Run the core pipeline in order to enable NepotismBridge detection:

```bash
ANALYSIS=<db_name>   # your analysis, from `analysis list`; must already exist

python3 scripts/pa_ingest.py              --analysis "$ANALYSIS"
python3 scripts/ch_backfill.py            --analysis "$ANALYSIS"
python3 scripts/normalize_officer_keys.py --analysis "$ANALYSIS"
python3 scripts/pa_link_officers.py       --analysis "$ANALYSIS"

urban-integrity-cli --analysis "$ANALYSIS" probe   # writes evidence-report.md
```

`evidence-report.md` (written to the working directory) records the latest probe verdicts for the selected
analysis. It is a list of patterns to review, not a list of findings — see [DISCLAIMER.md](DISCLAIMER.md).
It names real entities when run on real data; do not commit or publish it.

## API

- **HTTP / GraphQL** — `http://localhost:8080/graphql` — via async-graphql + Axum  
- **GraphiQL IDE** — `http://localhost:8080/graphiql`  
- **Health check** — `http://localhost:8080/health`  
- **Metrics** — `http://localhost:8080/metrics`  
- **gRPC** — `localhost:8081` — defined in `crates/api/proto/integrity.proto`
- **Analyst workbench** — `http://127.0.0.1:8090` (separate `workbench` process; UI plus `/api/*` routes, merged onto the same `api` router)

## Performance (v1.0 Verified)

| Metric | Target | Achieved |
|--------|--------|---------|
| Ingest-to-Alert P99 | < 500ms | ~3ms |
| 4-hop graph traversal | < 10ms | sub-1ms |
| 10k concurrent CSR reads (P99) | < 10ms | **2.5µs** (×4,300 headroom) |
| CSR snapshot build (100k nodes) | < 100ms | sub-10ms |

## Test Coverage

A full gated workspace run on 2026-09-16 reported **2,573 passed, 0 failed, 11 ignored** in about 637s wall-clock. The DB-gated tests need a live SurrealDB instance; they cover `surreal_integration`, the analysis lifecycle, cursor/backfill, CLI DB paths, the probes, the population oracle, the workbench, and enricher integration. Live external-API tests are gated separately behind `URBAN_LIVE_TESTS=1` and are **not** run in CI (they hit Companies House / Contracts Finder).

Those counts drift with every merge — re-measure rather than quoting them:
`cargo test --workspace 2>&1 | grep -E '^test result' | awk '{p+=$4; f+=$6; i+=$8} END {print p, f, i}'`.
If you need the DB-gated *count* (this README no longer quotes one — it had gone stale and was being
repeated as fact), derive it by attributing each gate expression to its enclosing `#[test]`/`#[tokio::test]`
function; grepping for gate *sites* over-counts, because the `db_tests_enabled()` and `skip_if_no_db()`
helper definitions match too. See [docs/TESTING.md](docs/TESTING.md) for the counting rule in full.

> **The DB gate is a skip-by-return, not `#[ignore]`.** A gated test that finds `URBAN_DB_TESTS` unset returns early and is *counted as passed*. `cargo test --workspace` and `URBAN_DB_TESTS=1 cargo test --workspace` therefore report the **same** passing count — a green run without the variable says nothing about whether the DB-gated tests exercised anything at all. This project has been bitten by exactly that before (gated tests recorded as done that had never once run), which is why CI always sets it. **Wall-clock is the only in-run evidence they ran:** the CLI binary (`tests/cli.rs`, 44 tests) takes ~0.13s ungated against ~440s gated on the live corpus (2026-09-16), and a whole-workspace gated run takes roughly 10–15 minutes. Earlier figures of ~1,400s for the CLI binary did not reproduce on 2026-09-16 and are not a baseline; a gated run finishing in *seconds* is the one to distrust.
>
> **A fast gated run is the thing to distrust, not a slow one.** `scripts/surreal-start.sh` derives its `WorkingDirectory` from its own location and `data/` is gitignored, so launching it from a git worktree silently creates a fresh, empty per-worktree store: every count matches, clippy is clean, and only the duration dissents. Before trusting a gated timing, confirm `systemctl --user show urban-surreal -p WorkingDirectory` is the repo root and that `INFO FOR ROOT` lists `urban_integrity`.

Every DB-gated test takes its namespace from the `urban-integrity-test-support` crate — `TestNs::connect(db)` for a connected handle with `SCHEMA_DEF` applied, or `NsGuard` when only the name is needed. Both **remove the namespace on drop**, so parallel and CI runs cannot collide and nothing accumulates in the local store. Hand-rolling a `format!("test_{uuid}")` namespace instead is what stranded thousands of them. `Drop` cannot fire when a test binary is SIGKILLed — sweep anything left behind with `scripts/surreal-drop-test-ns.sh` (dry-run by default; it only ever touches names starting with `test`).

```bash
# Base suite (no external services)
cargo test --workspace

# With live SurrealDB (adds the DB-gated integration tests)
scripts/surreal-start.sh   # isolated, memory-capped rocksdb instance on ws://localhost:8000
URBAN_DB_TESTS=1 cargo test --workspace

# One-shot local CI gate (fmt + clippy + tests, mirrors CI)
scripts/check.sh --db

# Coverage — HTML report
URBAN_DB_TESTS=1 cargo llvm-cov --workspace --html
xdg-open target/llvm-cov/html/index.html
```

### Build-enforced query-shape guard

`crates/core/tests/query_shape_guard.rs` is a source-level test: it scans non-test Rust sources under `crates/**` for SQL string literals and fails the build on either of the two SurrealDB 3.x query shapes that lose rows *silently* —

1. an indexed `kind` equality paired with an unindexed `CONTAINS` (the planner takes the index path and returns a truncated, run-to-run non-deterministic row set, while `count()` on the identical predicate stays correct); and
2. a large bare `LIMIT` with no paging (a ceiling sized to a past graph silently becomes a sample).

Both defects under-report *successfully* — the query returns `Ok` with fewer rows than exist and no caller has any signal — so no runtime assertion can catch them; making the shape impossible to write is the only reliable defence. It ships with its own detector tests pinned against the exact shapes measured in production (30 `#[test]` functions in the file), and runs in the base suite — no database required.

**It now watches nine tables, not one.** `WATCHED_TABLES` carries a per-table `LIMIT` threshold and the list of that table's indexed fields: `graph_events` (1,000), `identity_links` (1), `enrichment_runs` (1), `oc_quota` (1), `land_flip_candidates` (1), `ppd_transactions` (1,000), `land_flip_runs` (1), `land_flip_gate1_survivors` (1) and `analyses` (2). The thresholds are not uniform on purpose, and two of them carry written "do not lower this" comments in the source — `analyses` allows a genuine `LIMIT 1` unique-key lookup on a `UNIQUE`-indexed column, and `ppd_transactions` keeps its threshold at 1,000 because its one `LIMIT 200` gate-1 join is scoped to a single exact address triple — an assessed, closed decision, not unfinished work.

**The allowlist is not empty: it holds eight entries, each with a written justification** and a test (`every_allowlist_entry_is_justified_and_live`) that rejects an empty reason or a fragment that no longer matches anything, so a stale exemption fails loudly instead of quietly widening the hole. All eight are deliberate: one existence check (`SELECT id … LIMIT 1` behind a `bool`), five *pre-fix baseline* queries kept alive inside tests that issue the broken shape and the fixed shape against the same seeded data and assert they disagree — rewriting them would delete the evidence the fix changes behaviour — and, since 2026-09-01, `persistence::oracle`'s two dedup primitives. A separate test (`allowlist_gained_no_entries_this_phase`) pins the count at the `ALLOWLIST_BASELINE`, so a new exemption is a deliberate, reviewed act rather than a silent one.

That baseline was **raised 6 → 8 by explicit decision, and it did not widen the guard**. The two oracle entries became visible only because `broken_rule`'s `count(` exemption was *narrowed* in the same change to stop waving through a row-materialising inner `SELECT`; the net effect is two previously invisible Rule-1 shapes now recorded, justified and re-checkable. Each of the two states its evidence — a live third-shape cross-check on `an_anglesey_aebaa785` that agreed exactly — and states explicitly that this is **a sample, not a proof**, because the planner bug is run-to-run non-deterministic. Re-run it for a new corpus or SurrealDB version.

> **A green guard is still not proof of a clean tree.** Both `push_if_sql` and `broken_rule` bail out unless the literal contains `from <watched table>`, so a row-losing `LIMIT` over any *other* table remains invisible by design. The five `LIMIT 100000` reads over `owns_property`, `offshore_link`, `trustee_of`, `donates_to` and `controls` — outside the watched set — have been replaced by a paged scan (`crates/core/src/persistence/scan.rs`); the guard did not catch them and would not catch their return. `count(` exempts a literal only when it contains no nested `SELECT`; a `count()` wrapping a row-returning subquery is still checked. Treat the guard as a ratchet against reintroduction, not as evidence that the sweep is complete.

Compliant call sites route through `urban_integrity_core::persistence::scan` (or the ingestor's `backfill::scan_events_by_type` where a re-authenticating session is needed): both keep `kind` out of the `WHERE` clause, filter in Rust, and page by rows actually received.

See [docs/TESTING.md](docs/TESTING.md) for the full test inventory.

## Continuous Integration

`.github/workflows/ci.yml` runs on every push to `main` and on all pull requests, using the toolchain pinned in `rust-toolchain.toml` (`1.96.0`):

- **`test` job** — installs `protoc` (needed by `crates/api/build.rs`), starts SurrealDB `v3.1.0-beta.2` via `surrealdb/setup-surreal@v2` (root/root on `:8000`), builds the workspace, and runs the **DB-gated** suite with `URBAN_DB_TESTS=1 cargo test --workspace`. The gated integration tests run in CI so they cannot silently rot.
- **`lint` job** — `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`. Any new clippy warning fails CI.
- **`wasm-build` job** — builds `workbench-dto` and `workbench-web` for `wasm32-unknown-unknown`, runs the workbench boundary guards (forbidden wasm dependencies, fixture containment, abbreviation glossary, Cosmograph shim drift) and a release `trunk build` with Trunk pinned at 0.21.14.

Two `test`-job details worth knowing before debugging a CI-only failure: the workflow sets
`RUSTFLAGS=-C debuginfo=0` (the enricher test binary statically links the whole surrealdb-core tree, and a
full-debuginfo link exhausts the standard runner's memory) and `RUST_TEST_THREADS=2` (the CLI integration
tests each spawn a subprocess against the shared SurrealDB instance, and a 2-core runner flakes above that).
It also pre-applies the schema with `cargo run -p urban-integrity-cli -- status` before the suite, so the
many CLI subprocess tests do not race on the same DDL. SurrealDB runs with `surrealdb_auth: true` to match
local posture — under `--unauthenticated` the session-expiry and write-refusal regressions cannot fail for
the reason they claim to test.

`scripts/check.sh` runs the same gates locally in one shot (`--fix` to run `cargo fmt --all` first, `--db` to
add the SurrealDB-gated suite).

## Configuration

All settings in `config.toml`. Sensitive values via environment variables prefixed `URBAN__` with double-underscore nesting:

```
URBAN__INGEST__CH_API_KEY=...                  # Companies House streaming API key (ingestor)
URBAN__ENRICHER__CH__API_KEY=...               # Companies House REST API key for PSC/Disqualified adapters (separate from ingest key)
URBAN__SURREAL__WS_URL=ws://...                # SurrealDB endpoint override
URBAN__ENRICHER__LAND_REGISTRY__API_KEY=...    # HMLR account key shared by the CCOD + OCOD adapters (optional)
URBAN__ANALYSIS=an_foo_deadbeef                # Select a named analysis database (equivalent to the global --analysis flag)
URBAN__ENRICHER__PPD__ENABLED=true             # Opt the HMLR Price Paid Data staging adapter in/out
URBAN__ENRICHER__ENRICH_INTERVAL_SECS=86400    # Background enricher cadence used by `serve`
URBAN__WORKBENCH__BIND_ADDR=127.0.0.1:8090     # Workbench bind address (the `workbench --bind` flag overrides it)
```

`[workbench].bind_addr` defaults to `127.0.0.1:8090`; precedence is `config.toml`, then
`URBAN__WORKBENCH__BIND_ADDR`, then `workbench --bind`. `AppConfig` rejects unknown keys, so a misspelt
`URBAN__*` section fails `AppConfig::load()` rather than being ignored.

The `[detector]` block holds `phoenix_window_days` (180), `phoenix_min_value` (10000.0) and `imd_csv_path` — all read by `serve`, which is the only process that runs detection. Until 2026-08-11 `serve` built its `DetectorConfig` from literals and ignored the whole block, running a 730-day Phoenix window with deprivation weighting off; `detector_config_from` now reads it, and an absent block falls back to the documented 180 days rather than silently widening. `imd_csv_path` points at a file `data/*` gitignores — regenerate it with `scripts/build_deprivation_ward_index.py --nation <nation> --lad <codes>`. The PPD adapter is OGL v3 open data and needs **no** API key; it is opt-in via the `[enricher.ppd]` section (`enabled = true`) and takes an **explicit** `postcode_districts` allowlist, because deriving the districts from `Location` nodes pulls in UK-wide registered-office addresses from CCOD/OCOD and passes essentially every PPD row in the country. The FCA Mutuals adapter needs no key either and is enabled by default.

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for the full variable reference.

## Local LLM (optional)

The `SummarizeAlert` gRPC endpoint (and `--summarize` on `report`/`verify`) calls Ollama at `http://localhost:11434/v1` (OpenAI-compatible) and expects a Gemma-family model. It **degrades gracefully** if Ollama is unavailable — summaries are skipped, never a hard failure.

## Known Gaps

- **Phoenix's failed-company universe is narrow; the undated-contract cap is largely closed.** 21.0% of Contracts Finder contracts in the live corpus carry no `date_published` — 1,264 of 6,014, re-measured 2026-08-25. The mapper's fallback to OCDS `release.date` was forward-only, but `scripts/cf_backfill_dates.py` has since repaired the existing rows, bringing this down from the 89% quoted while that work was outstanding. Separately, the CH company-profile stream only emits companies that *change*, so the dissolved corpus is a single bulk-dissolution day; widen it with `scripts/ch_cessation_backfill.py` or by running the stream continuously.
- **Officer identity is only as good as what Companies House publishes.** Officers are keyed
  `director:{slug}` on the normalised name, which is what lets one human be one node across companies.
  `backfill officers` now reads the CH officer id and `{month, year}` date of birth and suffixes the key when
  they disagree, and `--audit`/`--split-collisions` repair keys written by earlier name-only tooling. But CH
  publishes **no** date of birth for company secretaries, so a key fusing two same-named secretaries cannot be
  refuted from CH data — and the month/year granularity cannot separate two people of the same name born in
  the same month.
- **Land-flip gate-3 cannot corroborate a free-text planning applicant.** No `identity_links` target a `director:`-keyed node, so an applicant that only exists as a name has an empty corroboration net and its candidates are correctly refused. `planning_identity` and `backfill ch` narrow this by giving applicants a real identifier; the auto-link tier is exact-name only, so abbreviated names (e.g. `"G Jones"`) are written for analyst review at 0.80 rather than auto-linked, and hundreds of matches are deliberately withheld pending review.
- **Address clusters double-count applicant name variants.** Planning applicants enter as free text, so `mr-a-mrs-…` and `mr-and-mrs-…` spellings of the same names at one address are one couple counted twice. The `director:unknown-applicant` placeholder is excluded from AddressCluster, but variant merging is still manual.
- **Registered societies are newly visible and unproven.** The FCA Mutuals adapter has shipped but has not been run against live data, so societies remain absent from existing analyses until an enrichment pass is run.
- **Deprivation weighting needs an index you must generate.** `data/*` is gitignored, so no deprivation CSV
  arrives with a clone and `serve` logs `IMD index load failed` until you run
  `scripts/build_deprivation_ward_index.py --nation <nation> --lad <codes>`. Separately, `serve` built its `DetectorConfig`
  from literals (including `imd_csv_path: None`) until 2026-08-11, so **every alert detected before that fix
  carries `m_env = 1.0` and a null `imd_rank`** regardless of the index, and older alerts carry a null
  `ward_code` and are invisible to `report --ward`. There is no re-detect-all command; the fix is forward-only.
- **`data_anomalies` has no retraction path.** An audit that stops reporting a finding leaves the old `open`
  row forever — the same defect `alerts withdraw` exists to solve on the alert side.
- **Planning portal coverage**: the sidecar implements the Idox PublicAccess, Swift/SwiftLG APAS and Arcus/Salesforce strategies; the WFS (Scotland Spatial Hub), PlanAPI and Northgate strategies are stubs that return empty results.
- **Full-scan cost after the query-shape sweep**: the paged replacements for the row-losing query shapes read the whole `graph_events` table instead of a truncated sample, so `entity`, `graph-path`, `verify` and the detectors are now correct but materially slower — the predicate is an unindexed `CONTAINS` over a table in the hundreds of thousands of rows. The durable fix is a schema change (an indexed node/edge-type column written at ingest, replacing the `CONTAINS` entirely); it is not yet implemented.
- **Query-shape guard covers nine tables, not the whole tree**: `query_shape_guard.rs` discards any SQL literal that does not name one of its `WATCHED_TABLES`, so row-losing `LIMIT` shapes over every other table are out of scope by construction. The five `LIMIT 100000` reads over `owns_property`, `offshore_link`, `trustee_of`, `donates_to` and `controls` now go through a paged scan (`persistence/scan.rs`), but the guard would not flag them if reintroduced. The `count(` exemption no longer covers a `count()` wrapping a nested `SELECT`. The guard passing does not mean the tree is clean.
- **Two schema kinds ship with no live data.** `disqualification` nodes and `declares_interest` edges were added on 2026-08-23 to fix two structurally unroutable edges, and no analysis has been re-enriched since. Measured 2026-08-25 on `an_anglesey_aebaa785`: `disqualification`, `disqualified_from` and `declares_interest` are 0 projected rows, and `graph_events` holds 0 `Disqualification`, `DisqualifiedFrom` and `DeclaresInterest` events — the WAL is empty of them too, so `project` alone will not populate them. Both need `enrich --source ch_disqualified` / `--source council_registers` re-run first.
- **`scripts/analyst_probe.py` has not been retired, and it is not equivalent.** All 22 probes and the report writer now ship in `urban-integrity-cli probe`; the Python script is kept as the differential reference and still has none of the verdict-vocabulary, provenance or gating fixes, so its `CLEAR` verdicts can still be claims about populations it never measured. Do not treat its `evidence-report.md` and the Rust one as interchangeable.
- **Workbench boot reads `graph_events` twice.** The canonical population is computed by one full read and the replay by a second, so the status page shows the two timestamps separately; on a live corpus they differ by the duration of the replay.
- **10 of the 22 probes have an independently recomputed population; 12 do not.** `probe-oracle` states the category and citation for each exclusion, but an excluded probe's declared population is still its own word for it.
- **`purge-orphans --apply` has never deleted a row on live data.** It ran against
  `an_anglesey_aebaa785` on 2026-08-22 and found **0 orphans**; the zero-orphan early return precedes the
  `--apply` branch, so the digest prompt and the whole delete path were unreachable and `--apply` was
  byte-identical to the dry run. `purge-ocod-owners` did run destructively (127,407 rows), which is how the
  shared backtick-quoting defect surfaced — but `purge-orphans`'s own delete path remains proven only by the
  DB-gated fixture tests. An earlier version of `purge-orphans` would have deleted the entire OCOD dataset.
- **Checkpointing has no call sites**: `write_checkpoint`/`hydrate_from_checkpoint` exist and are unit-tested, but `config.graph.checkpoint_path`, `checkpoint_events_n` and `checkpoint_interval_secs` are inert — `serve` rehydrates by replaying `graph_events`.
- **No retention automation**: the retention and pseudonymisation periods in `docs/gdpr-basis.md` are policy only. The manual deletion paths are `links reject` (per link) and `analysis remove --apply` (whole analysis).

## Development

```bash
cargo build --workspace                                          # Build all crates
cargo test --workspace                                           # Run the suite with the DB-gated tests skipping by return
URBAN_DB_TESTS=1 cargo test --workspace                          # Same tests and the same pass count — but the DB-gated ones now hit SurrealDB (~10–15 min)
scripts/build-workbench.sh                                       # Build the workbench frontend (Trunk + wasm32) into crates/workbench/dist/
scripts/check.sh                                                 # Local CI gate: fmt + clippy -D warnings + tests
cargo bench -p urban-integrity-detector --bench motif_detectors  # Motif traversal micro-bench
cargo bench -p urban-integrity-core --bench concurrent_readers   # Concurrent read bench
```

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for contributor workflow.

## Documentation

| Doc | Covers |
|-----|--------|
| [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) | Prerequisites, install, first run |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Contributor workflow, build commands, code style |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate boundaries, graph engine, data flow |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Full `config.toml` and `URBAN__*` variable reference |
| [docs/TESTING.md](docs/TESTING.md) | Test inventory, DB gating, namespace hygiene |
| [docs/API.md](docs/API.md) | GraphQL schema and gRPC service surface |
| [docs/PROJECTION.md](docs/PROJECTION.md) | How `graph_events` projects into native tables |
| [docs/CANONICAL-POPULATION.md](docs/CANONICAL-POPULATION.md) | The ward-reachable population shared by `export-graphml` and the workbench |
| [docs/ADJUDICATIONS.md](docs/ADJUDICATIONS.md) | The settled answers to the Python-vs-Rust probe disagreements (ADJ-01..03) |
| [docs/gdpr-basis.md](docs/gdpr-basis.md) | Lawful basis, retention and pseudonymisation policy |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Licensing terms for contributions and the CLA |

## License

Licensed under the **GNU Affero General Public License v3.0 or later** (`AGPL-3.0-or-later`) — see
[LICENSE](LICENSE). The `urban-integrity-schema` crate is additionally available under Apache-2.0
(`Apache-2.0 OR AGPL-3.0-or-later`). Contributions require a signed CLA; see [CONTRIBUTING.md](CONTRIBUTING.md).
