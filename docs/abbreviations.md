# UrbanIntegrity Engine — Abbreviations & Acronyms

A glossary of acronyms, abbreviations, and short codes used across the
UrbanIntegrity Engine codebase, planning docs, and data sources.

> Project: a real-time streaming audit engine for UK public-procurement
> corruption detection (Rust workspace, Tokio, SurrealDB-backed HIN graph).

---

## Data sources & domain bodies

| Abbr. | Meaning | Notes |
|-------|---------|-------|
| **CH** | Companies House | UK company registry. Streaming API (`stream.companieshouse.gov.uk`) + REST API (`api.companieshouse.gov.uk`) — **keys are typed**, a REST key 401s on streaming. |
| **CF** | Contracts Finder | UK public-procurement notices service (OCDS REST API). |
| **EC** | Electoral Commission | Source of political-donation records. |
| **ICIJ** | International Consortium of Investigative Journalists | Offshore Leaks / offshore-entity data. |
| **ONS** | Office for National Statistics | Demographics / planning-portal context. |
| **HMLR** | HM Land Registry | Property ownership + price-paid data. |
| **ROE** | Register of Overseas Entities | UK register of overseas owners of property; entities carry an `OE######` number (node key `GB-ROE:OE######`). |
| **PSC** | Persons with Significant Control | CH beneficial-ownership records. |
| **OpenSanctions** | — | External sanctions/PEP screening service (run via the `yente` matcher). Retained as a compliance class. |
| **OSINT** | Open-Source Intelligence | The enrichment layer that joins external public data into the graph. |
| **PEP** | Politically Exposed Person | Sanctions/screening category. |
| **anomaly** | Data-quality finding (`data_anomalies`) | **Not an alert.** An alert is a suspected corruption motif about the world; an anomaly is a defect in our own data (e.g. `officer_key_collision`). Surfaced by `anomalies list\|show\|review`. |
| **LPA** | Local Planning Authority | Council-level planning jurisdiction (graph keys are cross-LPA). |
| **IMD** | Index of Multiple Deprivation | Deprivation index (gov.uk CSV), used to weight scores. Per-nation: the Welsh equivalent is WIMD. `data/*` is gitignored, so no index ships with a clone — build one with `scripts/build_deprivation_ward_index.py --nation <n> --lad <codes>`, or alerts carry `m_env = 1.0` and a null `imd_rank`. |
| **CC** | Charity Commission | Register of Charities; bulk `publicextract.*` data on Azure blob storage. |
| **OC** | OpenCorporates | Cross-jurisdiction company registry aggregator (ODbL; quota-gated). Not to be confused with the `OC` CH company-number prefix below. |
| **FCA** | Financial Conduct Authority | Publishes the **Mutuals Public Register** — the whole register as one unauthenticated CSV on Azure blob storage. Source for registered societies, which are invisible to Companies House. |
| **GSS** | Government Statistical Service (code) | ONS geography code, e.g. a Welsh ward is `W05` + six digits. One uppercase letter + 8 digits — the ward-code format `analysis create` validates and hashes. |

**Registered society** — a corporate body that is **not a company**, constituted under the
Co-operative and Community Benefit Societies Act 2014 and registered with the FCA rather than
Companies House. Covers co-operatives and community benefit societies. Because they hold no CH
company number, they are absent from every CH-keyed join; the Mutuals adapter exists to close
that blind spot. `Society Status` is `Registered` or `Deregistered` (the latter being the
society-world analogue of a dissolved company).

## Scraping & portal platforms

| Name | Meaning |
|------|---------|
| **Scrapling** | Python fetching/parsing library behind the sidecar (`scrapling-sidecar/`, FastAPI on `localhost:8765`, **not** in the Cargo workspace). Ingestion degrades with a WARN if it is down. |
| **ModernGov** | Council committee-management platform scraped for registers of members' interests. The council-registers adapter holds a hardcoded LPA-code → ModernGov-slug map. |
| **Arcus** | Salesforce-based planning-portal platform (the `arcus` planning adapter targets it). |

## Licensing & compliance

| Abbr. | Meaning | Notes |
|-------|---------|-------|
| **OGL** | Open Government Licence (v3.0) | Licence on most UK public data used here (CH, HMLR, Gazette, EC). Attribution required. |
| **ODbL** | Open Database Licence (1.0) | Share-alike licence on ICIJ Offshore Leaks and OpenCorporates. Drives the `icij_*` table-prefix isolation from OGL sources. |
| **CC-BY-NC** | Creative Commons Attribution-NonCommercial 4.0 | OpenSanctions licence; requires `non_commercial_confirmed = true` or the adapter refuses to run. |
| **GDPR** | General Data Protection Regulation (UK GDPR) | Lawful basis per source recorded in `docs/gdpr-basis.md`. |
| **DPIA** | Data Protection Impact Assessment | UK GDPR Article 35 screening; per-source assessments in `docs/gdpr-basis.md`. |
| **PPERA** | Political Parties, Elections and Referendums Act 2000 | Statutory basis for the Electoral Commission donations register (s.62). |
| **CDDA** | Company Directors Disqualification Act 1986 | Statutory basis for the CH disqualified-directors register and its **two**-field minimum (name + DOB year scores 0.90, above the 0.85 auto-link threshold; name alone is discarded). Said "three-field minimum invariant" until 2026-09-03 — `gdpr-basis.md` retracted that on 2026-08-23 and this entry outlived the retraction, as did the in-code comments in `ch_disqualified.rs`: only the one at line 143 has been corrected ("INVARIANT: TWO-field minimum"); the module docs (lines 7, 9, 23), the scoring-function doc (lines 127, 129) and the inline comments at lines 403 and 737 still say "three-field minimum". |
| **TOS** | Terms of Service | |

## CH company-number prefixes

| Prefix | Meaning |
|--------|---------|
| **SC** | Scottish-registered company |
| **NI** | Northern Ireland-registered company |
| **OC** | LLP (Limited Liability Partnership), England & Wales |
| (none) | England & Wales limited company (8 digits, may be `00`-padded) |

## Data formats & standards

| Abbr. | Meaning |
|-------|---------|
| **OCDS** | Open Contracting Data Standard (Contracts Finder JSON) |
| **NDJSON** | Newline-Delimited JSON (CH streaming format) |
| **JSON** | JavaScript Object Notation |
| **TOML** | Tom's Obvious, Minimal Language (`config.toml`) |
| **YAML** | YAML Ain't Markup Language (CI workflow, `.github/workflows/ci.yml`) |
| **CSV** | Comma-Separated Values (IMD deprivation data, HMLR bulk extracts) |
| **CCOD** | Commercial and Corporate Ownership Data (HMLR — UK companies) |
| **OCOD** | Overseas Companies Ownership Data (HMLR — overseas owners) |
| **PPD** | Price Paid Data (HMLR — land/property transactions) |
| **PAON** | Primary Addressable Object Name (UK address-matching field) |
| **SAON** | Secondary Addressable Object Name (flat/unit within a PAON) |
| **GraphML** | XML graph-interchange format (the `export-graphml` CLI subcommand) |
| **ISO** | International Organization for Standardization — dates on the wire and in the address bar are ISO-8601 (`2021-06-30`). Added by plan 45-16: the entity profile's rejected-request state renders the workbench's own refusal sentence, which names the format, so the term now reaches the screen. |

## Architecture & engineering

| Abbr. | Meaning | Notes |
|-------|---------|-------|
| **HIN** | Heterogeneous Information Network | The core graph model (typed nodes + typed edges). |
| **WAL** | Write-Ahead Log | Durability channel: every mutation is logged before/while applied, then flushed to SurrealDB. |
| **CSR** | Compressed Sparse Row | Cache-friendly static graph layout for read-heavy traversal (ArcSwap snapshots). |
| **CQRS** | Command Query Responsibility Segregation | Write path (events) vs. read path (`project` into query tables). |
| **DDL** | Data Definition Language | SurrealDB schema definitions. |
| **AST** | Abstract Syntax Tree | Used by the graphify code-knowledge-graph extraction. |
| **API** | Application Programming Interface | |
| **gRPC** | gRPC Remote Procedure Call | tonic-based service surface. |
| **GQL** / **GraphQL** | Graph Query Language | async-graphql surface. |
| **REST** | Representational State Transfer | HTTP API style. |
| **HTTP/HTTPS** | HyperText Transfer Protocol (Secure) | |
| **WS** | WebSocket | SurrealDB connection (`ws://localhost:8000`). |
| **CLI** | Command-Line Interface | clap-derive; primary analyst interface. |
| **UI** | User Interface | |
| **SDK** | Software Development Kit | |
| **LLM** | Large Language Model | Local Gemma via Ollama (alert summarisation). |
| **MCP** | Model Context Protocol | Tooling integration (e.g. graphify MCP server). |
| **LSP** | Language Server Protocol | |
| **UUID** | Universally Unique Identifier | Used to isolate test namespaces in the DB-gated suites. |
| **SHA-256** | Secure Hash Algorithm, 256-bit | Ward-set hashing for content-addressable analysis DB names; also the pseudonymisation function in the retention policy. |
| **RFC 3339** | IETF date-time format profile | How `created_at`/`retired_at` are stored in the `_catalog` registry — as **strings**, never coerced `datetime` (SurrealDB 3.x silently drops rows on failed string→`datetime` coercion). |
| **MSRV** | Minimum Supported Rust Version | **Not declared or enforced here** — no `rust-version` key exists in `Cargo.toml` or any of the 13 workspace-member crate manifests, so the "1.87+" floor quoted in prose is aspirational, not a build constraint. What *is* pinned is the toolchain: 1.96.0 via `rust-toolchain.toml`, the single source of truth for local dev and CI. |
| **CI** | Continuous Integration | GitHub Actions (`.github/workflows/ci.yml`): `test` job (DB-gated suite) + `lint` job (fmt + clippy). |
| **CD** | Continuous Deployment / Delivery | (term only; no deployment pipeline configured.) |
| **OOM** | Out Of Memory | Storage-engine failure mode that drove the surrealkv→RocksDB migration. |
| **RAM** | Random-Access Memory | |
| **TTL** | Time To Live | The SurrealDB auth-token expiry window (~1h) that forces the re-auth calls in `migrate`/`enrich` on long runs. There is no prompt cache in this codebase — that clause was wrong. |
| **DoS** | Denial of Service | Threat class guarded against in the PPD staging adapter (bounded ingest). |

## Statistics & detection

| Abbr. | Meaning |
|-------|---------|
| **P_c** | Corruption-probability score (T-Digest normalised, IMD-weighted) |
| **T-Digest** | Probabilistic data structure for streaming quantile estimation |
| **BFS** | Breadth-First Search (e.g. the NepotismBridge traversal) |
| **DOB** | Date of Birth (used in corroborated, exact identity matching) |

### Detector motifs

| Motif | Meaning |
|-------|---------|
| **Phoenix** | A dissolved company's directors re-appearing in a new company (cross-LPA "phoenix" pattern) |
| **AddressCluster** | Many entities sharing one address |
| **NepotismBridge** | A 2-hop path linking a political actor to a company directorship |
| **Land Flip** (FLIP) | Pre-permission land purchase ahead of a planning decision |

## Entity-ID prefixes (graph node keys)

| Prefix | Meaning |
|--------|---------|
| **GB-COH:** | Great Britain — Companies House company (by company number) |
| **GB-OCOD:** | Overseas company owner from HMLR OCOD (key form `GB-OCOD:<country>-<id>`) |
| **GB-ROE:** | Overseas entity on the UK Register of Overseas Entities (key form `GB-ROE:OE######`) |
| **GB-MUT:** | FCA-registered society, keyed by its Mutuals Public Register registration number (`GB-MUT:{registration}`) |
| **CF:** | Contracts Finder contract node, keyed by the OCDS `ocid` (`CF:{ocid}`) |
| **CF-PARTY:** | Contracts Finder party node, keyed by the OCDS party id — so the full key looks like `CF-PARTY:GB-CFS-260069` |
| **planning:** | Planning-application node, keyed by the portal's application reference (`planning:{reference}`) |
| **ward:** | Ward node, keyed by GSS ward code (`ward:{ward_code}`) |
| **LOC:** | Location node from **address normalisation** — `LOC:{postcode}:{house_number}` |
| **location:** | Location node from the **planning mapper** — `location:{address}`, lowercased and hyphenated |
| **PA:** | Political **party** as a donation recipient — `PA:{party_slug}`, minted by the Electoral Commission adapter (`electoral_commission.rs:560`). NOT councillors, despite the older "e.g. councillor" gloss here |
| **political_actor:** | Political actor node for a **councillor** — `political_actor:{slug}`, minted from scraped ModernGov registers (`council_registers.rs:315`) |
| **OFF:** | Officer (company director/officer) |
| **director:{slug}** | Director, keyed by normalised name slug (enables cross-LPA Phoenix dedup). May carry an evidence suffix: `-b{YYYYMM}` for a Companies House date of birth (e.g. `director:smith-jane-b197006`), or `-o{officerid}` where CH publishes no DOB and two officer ids must be split. See `crates/ingestor/src/backfill/officers.rs`. |
| **psc:** | Person with Significant Control node (`crates/enricher/src/adapters/ch_psc.rs`) |
| **charity:** | Registered charity (Charity Commission) |
| **property:** | Land title (HMLR CCOD/OCOD) |
| **sanctions:** | OpenSanctions entity |

> **Two disjoint location keyspaces exist.** `LOC:` (address normalisation, postcode + house
> number) and `location:` (planning mapper, hyphenated address string) key the same kind of
> real-world thing under different schemes, and nodes under one will never join to nodes under
> the other. Same hazard as the two near-identical slug functions — `naming::slugify` joins on
> `_`, `applicant_slug` on `-` — where picking the wrong one silently creates a third disjoint
> keyspace. Check which one a code path uses before keying anything new.

> **`GB-CFS:` is not a prefix.** `GB-CFS-260069` is an OCDS *identifier* (hyphen, not colon)
> that appears **inside** a key, never at its head: the planning mapper wraps it as
> `CF-PARTY:GB-CFS-260069`. An earlier version of this glossary listed `GB-CFS:` as a node-key
> prefix; no such prefix exists anywhere in the codebase.

> Not entity-ID prefixes: `PSC:`, `CC:`, `EC:`, `ICIJ:`, `PPD:`, `CCOD:`, `OCOD:` and `SKIP:`
> also appear followed by a colon, but those are **log-message** prefixes naming the emitting
> adapter (or, for `SKIP:`, a DB-gated test skipping itself). They never key a graph node.

## Analysis lifecycle (v5.0)

| Term | Meaning |
|------|---------|
| **analysis** | One investigation, isolated in its own SurrealDB database under the shared `urban_integrity` namespace |
| **`an_<slug>_<hash8>`** | Content-addressable analysis DB name — label slug + the first 8 hex chars of the SHA-256 over the sorted, deduped GSS ward codes (e.g. `an_example_00000000`) |
| **`_catalog`** | Registry database holding the SCHEMALESS `analyses` table — the source of truth for which analyses exist |
| **ward set** | The list of GSS ward codes defining an analysis's scope; identical ward sets hash to the same DB name (the idempotency guard) |
| **baseline** | An existing DB cloned into a new analysis via `--from` (`surreal export \| surreal import`, carries ingest cursors) |
| **reserved names** | `main`, `uk_national` — never creatable or removable as analysis targets |

## Planning / workflow (GSD framework)

| Abbr. | Meaning |
|-------|---------|
| **GSD** | "Get Shit Done" — the phase-based planning workflow (kept in the project's planning notes) |
| **ADR** | Architecture Decision Record |
| **PRD** | Product Requirements Document |
| **SPEC** | Specification |
| **TDD** | Test-Driven Development |
| **E2E** | End-to-End (testing) |
| **PR** | Pull Request (GitHub; phases ship via merge PRs) |

## Requirement category codes

Each numbered requirement (e.g. `ACT-01`) is prefixed by a category code. The **active** milestone's
requirements live in a top-level requirements file; on milestone completion that file is archived to
a per-milestone requirements file. So look in both places: the top-level file is the
milestone in progress, the `milestones/` copies are everything shipped.

The tables below list, per milestone, the families that file actually **defines** — counted from
its bold `**CODE-NN**` requirement headings. A code mentioned in prose is not a family: several
files cross-reference codes owned by another milestone (v4.0 cites `ENR-11`, v2.0 lists
`INPP-05` as deferred, v6.0 back-references `FLIP-02`), and reading those as spans sends you to
the wrong file. **POL**, **PROP** and **VRD** each genuinely span two milestones (VRD: v7.0 and
v8.0).

**v9.0 (Fraud Analyst Workbench — active, phases 42–50):**
| Code | Category | Count |
|------|----------|-------|
| **WBX** | Workbench foundation & contracts | 8 |
| **FRB** | Finding board & motif rendering | 8 |
| **PQT** | Parquet export & Cosmograph | 6 |
| **PIV** | Entity profile & pivot | 6 |
| **NAV** | Navigation, Home & Help | 6 |
| **GRA** | Graph analysis | 6 |
| **EVM** | Evidence model | 6 |
| **TRI** | Alert & motif triage inbox | 5 |
| **RVQ** | Review queues & evidence export | 5 |
| **CNV** | Graph canvas | 4 |
| **JRN** | Journalist / NGO tier *(deferred)* | 3 |
| **DBT** | Audit-trail debt *(deferred; carried from v8.0)* | 3 |
| **PYR** | Python retirement *(deferred; carried from v8.0)* | 2 |

**v8.0 (Independent Verification & Verdict Gating — shipped 2026-09-04, phases 37–41):**
| Code | Category | Count |
|------|----------|-------|
| **ORC** | Oracle / independent recomputation | 8 |
| **GAT** | Verdict gating | 6 |
| **VRD** | Verdict correctness *(also a v7.0 family)* | 4 |
| **ADJ** | Differential adjudication | 4 |
| **UI** | Analyst-facing interface *(deferred)* | 3 |
| **PYR** | Python retirement | 2 |
| **DBT** | Tech debt | 2 |

**v7.0 (Rust Probe Port & Verdict Correctness — shipped 2026-08-31, phases 31–36):**
| Code | Category | Count |
|------|----------|-------|
| **VRD** | Verdict correctness *(also a v8.0 family)* | 9 |
| **PRB** | Probe battery | 8 |
| **GRD** | Query-shape guard | 10 |
| **RPT** | Report assembly | 5 |

**v6.0 (Data Quality & Compliance — shipped 2026-08-23, phases 24–30):**

| Code | Category | Count |
|------|----------|-------|
| **IDN** | Planning-data identity (applicant attribution) — Phase 24 | 5 |
| **PHX** | Phoenix enablement: officers & company status — Phase 26 | 5 |
| **INT** | Data integrity (purge, canonicalisation, record ids) — Phase 28 | 5 |
| **LFA** | Land-flip address matching — Phase 29 | 6 |
| **RET** | Retention & compliance (GDPR enforcement, DPIAs) | 6 |

> Phase numbering in this milestone was renumbered mid-flight: Data Integrity moved from 26 to
> 28 on 2026-07-28 when Phoenix Enablement took 26. Phase 29 was appended 2026-08-05 and Phase
> 30 after it. A phase number quoted in an older doc may not mean what it means now.

**v4.0 (Data Activation — shipped 2026-06-20):**

| Code | Category | Count |
|------|----------|-------|
| **FLIP** | Pre-Permission Land Flip | 5 |
| **SCH** | Schema (self-documentation) | 4 |
| **ACT** | Data Activation | 3 |
| **POL** | Political / EC donations normalisation *(also v3.0)* | 3 |
| **PROP** | Overseas Ownership / Property *(also v3.0)* | 3 |
| **OPS** | Operational Activation | 2 |

**v3.0 (OSINT Enrichment milestone):**

| Code | Category | Count |
|------|----------|-------|
| **ENR** | Enrichment | 11 |
| **FUZ** | Fuzzy matching *(category retired — exact-match-only policy enforced 2026-06-13; the FUZ-numbered adapters themselves shipped and remain, now exact-join-only)* | 7 |
| **SURF** | Surfacing (analyst-facing results) | 5 |
| **REV** | Review | 5 |
| **POL** | Political / EC donations normalisation *(also v4.0)* | 5 |
| **PROP** | Overseas Ownership / Property *(also v4.0)* | 4 |
| **CHA** | Charity (Commission) | 4 |

**v2.0 (CLI analyst platform):**

| Code | Category | Count |
|------|----------|-------|
| **ENT** | Entity (query/lookup) | 6 |
| **REP** | Report (integrity report) | 6 |
| **ALT** | Alerts (inbox, lifecycle) | 5 |
| **CRS** | Cross-source corroboration | 5 |
| **VER** | Verification | 5 |
| **GQL** | GraphQL surface | 5 |

**v1.0 (engine foundations):**

| Code | Category | Count |
|------|----------|-------|
| **SCHM** | Schema — node/edge types | 6 |
| **GRPH** | Graph engine (petgraph hot graph, CSR snapshots) | 6 |
| **INCH** | Ingest: Companies House | 6 |
| **MOTF** | Motif detectors (Phoenix, AddressCluster, NepotismBridge) | 5 |
| **STAT** | Statistics / scoring (`P_c`, T-Digest, IMD weighting) | 5 |
| **API** | API surface (Axum serving gRPC + GraphQL) | 5 |
| **INCF** | Ingest: Contracts Finder | 5 |
| **INPP** | Ingest: Planning Portals *(v1.0 only — `INPP-05` is recorded as deferred in v2.0, which is not the same as being a v2.0 family)* | 5 |
| **PROJ** | Project scaffolding (Cargo workspace layout) | 4 |
| **PERF** | Performance | 4 |
| **CLI** | CLI interface (clap) | 4 |
| **ADDR** | Address normalisation | 3 |

Note `API` and `CLI` appear twice in this glossary with different senses: as general acronyms
under *Architecture & engineering*, and here as v1.0 requirement-code families.

The post-v4.0 **analysis-lifecycle** work (v5.0) is tracked as design/plan documents
(the analysis-lifecycle spec and its companion design specs) and shipped via
PRs #16–#17 rather than numbered requirement codes.

## Land-Flip detector: `Tier` and `Severity` are two different enums

`Tier` below is the **temporal window** only, stored in `land_flip_candidates.tier`
(`land_flip.rs:57-62`). It is not a severity. `Severity` is a separate enum with exactly two
variants, `Medium` and `Low`, and **deliberately no `High`** (`land_flip.rs:67-81`) — the MEDIUM
ceiling the module header calls the product's safety contract. This section was headed "Severity
tiers" until 2026-09-03, which hid that ceiling behind a word the code reserves for the other enum.

| Tier | Window |
|------|--------|
| **TIER_1** | Inside the planning-application window |
| **TIER_2** | 0–12 months pre-submission |
| **TIER_3** | 12–24 months pre-submission |

---

*Generated 2026-06-13; updated 2026-06-21 (v4.0 Data Activation — added ROE, GB-OCOD:/GB-ROE:
prefixes, CI/CD, OOM, DoS, CSV/YAML, and others); reviewed 2026-07-26 (v5.0 analysis lifecycle —
added the licensing/compliance section (OGL, ODbL, CC-BY-NC, DPIA, PPERA, CDDA), GSS, CC, OC,
SHA-256, RFC 3339, the analysis-lifecycle vocabulary, and corrected the requirement-codes
section to the per-milestone requirements-file layout); updated again
2026-07-26 when milestone v6.0 opened — an **active** milestone restores a top-level
requirements file, so the earlier "there is no single top-level requirements file" note was
only true between milestones; added the v6.0 codes (IDN, RET, INT).*

*Updated 2026-08-14 against `main` @ `bdf6f0c`. Added the FCA / Mutuals Public Register and
registered societies; the scraping-platform section (Scrapling, ModernGov, Arcus); GraphML; the
six missing entity-ID prefixes (`GB-MUT:`, `CF:`, `planning:`, `ward:`, `LOC:`, `location:`)
plus the two-keyspace and log-prefix warnings; removed `GB-CFS:`, which never existed as a
prefix. Corrected the MSRV entry (no `rust-version` key is declared anywhere, so the 1.87 floor
is prose only) and the v6.0 phase range 24–26 → 24–30, with a note on the mid-milestone
renumbering. Rebuilt the requirement-code section from the bold `**CODE-NN**` headings each
milestone's requirements file actually defines, with per-family counts: added the two missing v6.0 families
(**PHX**, **LFA**) and the twelve v1.0 families that had never been listed, and split the merged
"v1.0–v2.0" row into real per-milestone tables.*

*A caution for whoever updates this next. The first pass at that rebuild derived families by
grepping `\b[A-Z]{2,5}-[0-9]{2}\b`, which matches any **mention** of a code — including prose
cross-references — and so invented three cross-milestone spans that do not exist (`ENR` and
`INPP` in v4.0, `INPP` in v2.0, `FLIP` in v6.0) and a "largest family" claim that was a tie.
Count definitions, not occurrences. The same pass missed the `location:` prefix because its
`format!` is split across lines, so a single-line grep for `format!("location:` returns nothing.*

*If you add a new acronym to the codebase or planning docs, add it here too.*
