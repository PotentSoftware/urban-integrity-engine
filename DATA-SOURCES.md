# Data sources and their licences

**No data ships with this repository.** `data/` is gitignored and stays that way.
Cloning the engine gives you the code and nothing else — you must obtain every
dataset yourself, under your own licence, for your own permitted purpose.

That is not an oversight. Several of the sources below prohibit redistribution
outright, and at least two restrict commercial use regardless of what licence
the *code* carries. The AGPL on this codebase says nothing about the data you
feed it.

> ⚠️ **The Status column below is a checklist, not a legal opinion.** Entries
> marked **VERIFY** have not been confirmed against current terms by a lawyer.
> Confirm each one, in writing, before any public deployment, any demo to a third
> party, or any commercial use. Terms change; several of these changed within the
> last two years.

## Source register

| Source | Used by | Licence (as understood) | Redistribute? | Commercial? | Status |
|---|---|---|---|---|---|
| Companies House streaming API | `ingestor` (CH stream) | CH API terms of use; data Crown copyright / OGL | Data: likely yes under OGL. API access: no sub-licensing | Believed yes | VERIFY |
| Companies House REST (PSC, disqualified) | `enricher/adapters/ch_psc`, `ch_disqualified` | CH API terms; per-key rate limits | No bulk redistribution of API responses | Believed yes | VERIFY |
| Contracts Finder (OCDS) | `ingestor` (CF poller) | Open Government Licence v3.0 | Yes, with attribution | Yes | CONFIRMED |
| HMLR **CCOD** (UK corporate ownership) | `enricher/adapters/land_registry_ccod` | HMLR licence, accepted per-licensee | **No** — licensee-bound | Requires separate/paid terms | **VERIFY — restrictive** |
| HMLR **OCOD** (overseas ownership) | `enricher/adapters/land_registry_ocod` | HMLR licence, accepted per-licensee | **No** — licensee-bound | Requires separate/paid terms | **VERIFY — restrictive** |
| HMLR **PPD** (price paid) | `enricher/adapters/land_registry_ppd` | Open Government Licence v3.0, attribution required | Yes, with the prescribed attribution notice | Yes | VERIFY |
| ICIJ Offshore Leaks | `enricher/adapters/icij` | ICIJ database terms; ODbL-derived, with use conditions | Conditional at best | **Believed restricted** | **VERIFY — restrictive** |
| OpenSanctions | `enricher/adapters/opensanctions` | Free for non-commercial; **commercial use requires a paid licence** | Per their terms | **No, not without a licence** | **VERIFY — restrictive** |
| OpenCorporates | `enricher/adapters/opencorporates` | Proprietary. Free tier by application for open-data/non-profit use; commercial is paid | No | **No, not without a licence** | **VERIFY — restrictive** |
| Charity Commission register extract | `enricher/adapters/charity_commission` | Open Government Licence | Yes, with attribution | Yes | VERIFY |
| Electoral Commission donations register | `enricher/adapters/electoral_commission` | Open Government Licence | Yes, with attribution | Yes | VERIFY |
| The London Gazette | `enricher/adapters/london_gazette` | Crown copyright; OGL for most content | Generally yes | Generally yes | VERIFY |
| FCA Mutuals Register | `enricher/adapters/mutuals_register` | Public register; FCA site terms | Likely limited | VERIFY | VERIFY |
| Council registers of interest | `enricher/adapters/council_registers` | **Per council.** No common licence. Scraped | Per council | Per council | **VERIFY per authority** |
| LPA planning portals (Arcus/Salesforce) | `ingestor` via `scrapling-sidecar` | **Per portal terms of use.** Scraped | Per portal | Per portal | **VERIFY per portal** |
| IMD / WIMD deprivation indices | `detector/src/scoring/imd.rs` | Open Government Licence | Yes, with attribution | Yes | CONFIRMED |
| ONS ward / LSOA boundaries and codes | `detector` scoring, `ingestor` | Open Government Licence | Yes, with attribution | Yes | CONFIRMED |

## What this implies, concretely

**1. A commercial deployment cannot use the full source set as-is.** OpenSanctions,
OpenCorporates and ICIJ each need a separate commercial licence or must be
switched off. Budget for this before quoting anyone a price. The adapters are
individually toggleable in `config.toml` for exactly this reason.

**2. A public demo must not serve the live corpus.** CCOD and OCOD are licensed to
a named licensee. Exposing their contents through a web UI to third parties is
onward disclosure, whatever the UI looks like. Demonstrate on the synthetic
planted fixture (`scripts/toy-analysis.sh`, analysis `an_planted_fixture_*`)
instead — it exercises every detector and carries no licence or personal-data
exposure at all.

**3. The output is personal data.** A finding names, or is trivially linked to, a
living person. Under UK GDPR you are the controller of that processing. Before
publishing or demonstrating any output derived from real data you need, at
minimum: a lawful basis, a completed DPIA, a retention position, and a process
for rectification when a finding is wrong. The project has already recorded at
least one confirmed false positive against a real named individual, recorded in
the project's planning notes. Assume there are more.

**4. Attribution is not optional under OGL.** Every OGL source above requires an
attribution statement in any published product. Collect them into whatever you
publish.

## Adding a new source

A new adapter is not complete until this table has a row for it. The row must
name the licence, state whether redistribution is permitted, state whether
commercial use is permitted, and link to the terms you actually read. "It's a
public website" is not a licence.

## Attribution notices

Contains public sector information licensed under the Open Government Licence
v3.0.

Contains HM Land Registry data © Crown copyright and database right. This data is
licensed under the Open Government Licence v3.0. *(Applies to PPD only — CCOD and
OCOD carry their own separate terms and are not covered by this notice.)*
