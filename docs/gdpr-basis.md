<!-- generated-by: gsd-doc-writer -->
# GDPR Lawful Basis — UrbanIntegrity OSINT Enrichment

This document records the lawful basis under UK GDPR for processing personal data from each OSINT source used by the UrbanIntegrity enrichment pipeline. Most sources rely on Article 6(1)(f) (legitimate interests); the statutory-register sources (Electoral Commission, Council Registers of Interest, OpenCorporates) rely primarily on Article 6(1)(e) (public task). The public anti-corruption accountability purpose forms the specific interest across all sources.

Originally skeletoned in Phase 16 for the v3.0 OSINT Enrichment milestone, it now also covers the property and political sources added since — Phases 18 and 20 (v3.0) and Phase 22 (v4.0). **Phase 23 (PPD + land-flip) is NOT yet covered; see *Adapters Without a Section* below.** (Corrected 2026-08-23: this line previously attributed Phases 18 and 20 to v4.0 and implied Phase 23 was covered.)

## How to Use This Document

Before a new OSINT adapter ships, its section in this document must be completed with concrete personal data field names, the specific retention period, and any licence compliance notes. The section's stub status must be replaced with confirmed detail. No adapter may merge to main until its section is marked complete and reviewed.

**Review 2026-08-23.** A verification pass against the shipped adapters found 11 factual errors in this document, most of them understating what is persisted, plus three adapters with no section at all. Every correction is marked inline with a dated `Corrected` note rather than silently rewritten, so the previous text remains auditable. The merge gate stated above has now failed twice (see the 2026-07-26 review below) — it is a documented rule with no enforcement mechanism, and three more adapters shipped without sections.

**Review 2026-07-26.** The three Phase 19 sources (Charity Commission, OpenSanctions, ICIJ) shipped while their sections still read "to be expanded before Phase 19 ships" — the gate above was not honoured at merge time. Those three sections have now been completed against the shipped code, and in all three cases the implemented behaviour processes **less** personal data than the original stubs anticipated, because the exact-match-only policy (2026-06-13) removed every name-based matching path. Where a stub listed fields the adapter never consults, that is recorded explicitly below rather than silently dropped.

## Retention Enforcement Status

**The retention and pseudonymisation periods in this document are policy commitments, not automated behaviour.** There is no retention job, TTL, or pseudonymisation routine in the codebase (`grep -r retention crates/ --include=*.rs` returns nothing). Deletion today is manual, at two granularities:

- **Per link:** `links reject` permanently suppresses an individual identity link from re-proposal (REV-04). ⚠ **This is suppression, not erasure** — it is an `UPDATE … SET status = 'rejected'` (`links.rs:243`). The row and its `evidence_json` (which for `ch_disqualified` holds DOB month and year) remain in the database indefinitely. Corrected 2026-08-23: listing it under "deletion" overstated the erasure capability of this system.
- **Whole analysis:** `analysis remove <db_name> --apply` drops an entire analysis database and its checkpoint directory, and marks the registry row `retired`. It refuses to act on an analysis still inside the Phoenix detection window (`detector.phoenix_window_days`, default 180) unless `--decommission` is passed. This is a coarse evidence-lifecycle control, **not** a GDPR retention mechanism — it deletes everything in the analysis, not just time-expired personal data.

Automating per-source retention (age-based pseudonymisation of `identity_links.evidence_json`) remains outstanding work. Until it exists, the periods below describe intent and the manual paths above are the only implemented deletion routes.

**Deliberately deferred as of 2026-07-28.** The automation work (Phase 25, requirements RET-01–06) is on hold indefinitely by operator decision: the engine currently runs locally against operator-controlled analysis databases that never leave the host, so retention automation would encode policy against a deployment topology that does not exist. **The hold is a scheduling decision, not a compliance judgement** — the periods below remain binding commitments, and the two DPIA preconditions still apply in full:

- **Before any live external deployment with real users**, per-source retention enforcement (RET-01–03) must exist. A deployment that processes third-party personal data with no enforced retention would breach the commitments in this document.
- **Independently of the hold**, neither OpenSanctions nor ICIJ output may inform a decision about an identified individual until that source's DPIA is completed and recorded here (RET-04, RET-05). Both sources are assessed medium risk. This gate is live today.

---

## Companies House — Persons with Significant Control (PSC)

**Status: Complete (Phase 17, ENR-10)**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is detecting undisclosed beneficial ownership and control relationships in public procurement supply chains, enabling local authorities and auditors to identify Phoenix company reuse and hidden conflicts of interest before contracts are signed. The processing is necessary for this purpose because the PSC register is the only authoritative public source of beneficial ownership data for UK companies, and cross-referencing it against procurement contracts cannot be achieved without linking to the individual PSC records. The data subjects' interests are not overridden because: (a) the PSC register is a statutory public register designed for exactly this scrutiny under the Companies Act 2006; (b) the data has already been published by the data subject under a legal obligation; and (c) processing is limited to the minimum fields needed for identity resolution and audit (see Data Minimisation below).

### Licence

Companies House Open Data Licence (OGL-derived, free to use). The PSC data is published under the Companies House Terms and Conditions which permit reuse for accountability purposes. Data available at: https://developer.company-information.service.gov.uk/

The streaming API endpoint used is: `https://stream.companieshouse.gov.uk/persons-with-significant-control` (HTTP Basic Auth, CH API key). No additional licence is required beyond the OGL terms.

### Personal Data Fields Processed

The following personal data fields are received from the CH PSC streaming API and processed by this system:

| Field | Source | Use | Persisted? |
|-------|--------|-----|-----------|
| `name` (full name) | PSC stream `data.name` | Identity resolution — a normalised slug is derived for officer matching | **Yes — the RAW full name.** `PscData.name` is the resolved display name (`ch_psc.rs:719-728`), projected verbatim into the `psc` table (`crates/ingestor/src/projector.rs:932-937`). The slug appears only in the record id (`psc:{company_number}-{name_slug}`) |
| `natures_of_control` | PSC stream `data.natures_of_control` | Surfaced to analysts as the nature of the beneficial ownership relationship | Stored on the `Psc` node |
| Company number (extracted from `resource_uri`) | PSC stream envelope | Links the PSC record to the correct company entity in the graph | Stored on the `Psc` node |

Note: `date_of_birth`, `nationality`, and `country_of_residence` are not modelled in `PscEventData`. These fields are not present in the data structure; serde silently ignores any such keys if they appear in the raw stream. Identity links use a fixed confidence of 1.0 for name+company matches — no DOB disambiguation is performed by the PSC adapter.

### Data Minimisation

> **Corrected 2026-08-23.** This section previously stated that the name is stored "as a normalised slug only … not as a verbatim full name". That was wrong, and it understated what is persisted. `PscData.name` holds the verbatim display name and the `psc` table stores it as published. The slug is used for the record id and for officer matching, not as a substitute for the name.

The `Psc` node carries the **full name as published**, the natures of control, and the company number extracted from the resource URI. No date of birth, nationality, or country of residence is modelled or stored — those fields are absent from `PscEventData`, so serde ignores them if present in the raw stream.

Minimisation therefore rests on field *selection* (three fields of the several the stream carries), not on pseudonymisation of the name. A PSC is by definition a natural person exercising significant control, so this is personal data held in the clear on a graph node.

### Retention

Identity link records (`identity_links` rows with `source='ch_psc'`) are retained for 5 years following the expiry or termination of the associated procurement contract, after which the `evidence_json` field is pseudonymised (replaced with a hash of the source identifiers).

> **Corrected 2026-08-23.** The previous text concluded that "the `Psc` node name slug (already normalised/pseudonymised) may be retained indefinitely as it does not constitute personal data in isolation". Both the premise and the conclusion fail: the node stores the raw name, not a slug, so it is personal data and an indefinite-retention carve-out cannot rest on that reasoning. **The `Psc` node name is in scope for the same retention commitment as the link evidence**, and RET-01's pseudonymisation work must cover node weights in `graph_events`, not only `identity_links.evidence_json`.

---

## Companies House — Disqualified Directors

**Status: Complete (Phase 17, ENR-10)**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is identifying persons who are legally barred from company directorships under the Company Directors Disqualification Act 1986 (CDDA 1986) but who nonetheless appear in procurement supply chains as active directors or beneficial owners, constituting a criminal offence and a direct risk to public funds. The processing is necessary for this purpose because the only reliable source of ban-window data is the CH disqualified officers register, and detecting active evasion requires matching disqualified persons against the procurement graph in near-real-time. The data subjects' interests are not overridden because: (a) the disqualification register is a statutory public register whose transparency is mandated by Parliament specifically to protect creditors and the public; (b) the processing concerns conduct already adjudicated by a court (the CDDA order); and (c) only the minimum data needed for identity resolution and ban-window verification is retained (see Data Minimisation below).

### Licence

Companies House Open Data Licence (OGL-derived, free to use). Disqualified officer data is published under the same Terms and Conditions as other CH data products. Data available at: https://developer.company-information.service.gov.uk/api/docs/disqualified-officers/

The streaming API endpoint used is: `https://stream.companieshouse.gov.uk/disqualified-officers` (HTTP Basic Auth, CH API key). No additional licence is required beyond the OGL terms.

### Personal Data Fields Processed

The following personal data fields are received from the CH disqualified officers streaming API and processed by this system:

| Field | Source | Use | Persisted? |
|-------|--------|-----|-----------|
| `forename`, `surname` | Disqualified stream `data` | Identity resolution — normalised name slug used to match against known officer nodes | Name slug only (normalised) |
| `date_of_birth` (month+year, format "YYYY-MM") | Disqualified stream `data.date_of_birth` | Disambiguation — increases matching confidence from 0.90 to 0.95 when month is present | DOB month+year in `evidence_json` on `identity_links` only |
| `disqualifications[].case_identifier` | Disqualified stream | Surfaces the CDDA 1986 case reference to analysts for independent verification | Stored in `DisqualifiedFromData.case_number` on the `DisqualifiedFrom` edge |
| `disqualifications[].disqualified_from` | Disqualified stream | Defines the start of the ban window for active-evasion detection | Stored in `DisqualifiedFromData.disqualified_from` |
| `disqualifications[].disqualified_until` | Disqualified stream | Defines the end of the ban window | Stored in `DisqualifiedFromData.disqualified_until` |
| `title` | Disqualified stream | Received but **not** persisted | No |

Note: `nationality` is not modelled in `DisqEventData` and is not present in the data structure.

### Name + DOB Minimum (CHA-02)

A `DisqualifiedFrom` identity link is only created when the name matches a known officer node **and at least the DOB year is present**. `assign_confidence` (`ch_disqualified.rs:140-151`) scores name + month + year at 0.95, name + year alone at 0.90, and returns `None` for a name-only match — so a name-only match produces no link at all. This prevents false disqualification accusations, a cardinal integrity rule for this system and equivalent in severity to a false criminal allegation.

> **Corrected 2026-08-23.** This section was headed "Three-Field Minimum Invariant" and stated that a link "is only created when at least name + DOB month + DOB year are all present". That is not what the code does: **name + year with no month scores 0.90, which is above `AUTO_LINK_THRESHOLD` (0.85) and auto-links.** The guarantee is a two-field minimum with a confidence penalty for the missing month, not a three-field minimum.
>
> The secondary claim — "a name-only match does not reach the 0.70 candidacy threshold and is discarded" — was also wrong in mechanism, though right in outcome: `assign_confidence` returns `None` for a name-only match, so no score is ever produced and no threshold comparison occurs.
>
> Note the in-code `INVARIANT` comment at `crates/enricher/src/adapters/ch_disqualified.rs:143` (in the confidence-scoring function) has been corrected and now reads `INVARIANT: TWO-field minimum — name alone is never enough (ENR-11)`. Other comments in the same file still say "three-field minimum" (the module doc at lines 7, 9 and 23, the doc comment at lines 127 and 129, and the comments at lines 403 and 737), which contradicts the match arms; those still need the same correction. This document no longer relies on any of them.

### Data Minimisation

Full date of birth is **not** persisted; only month and year (as published) are stored transiently in `evidence_json` for the identity_link confidence record. The day component is not present in the disqualified officers register for natural persons. The `evidence_json` is stored as a flat JSON string. Title is not persisted. The `DisqualifiedFrom` edge carries only the ban window and case number — no personal data beyond the case reference (which is a court-order identifier, not a biometric or sensitive category).

### Retention

`DisqualifiedFrom` edge records and associated `identity_links` rows (with `source='ch_disqualified'`) are retained for the duration of the disqualification ban plus 5 years, or until the associated procurement context is archived (whichever is later), after which `evidence_json` is pseudonymised. The ban window dates and case number are retained beyond pseudonymisation as they are court-order metadata, not personal data in their own right under UK GDPR.

---

## Land Registry — CCOD Bulk CSV

**Status: Complete (Phase 18, ENR-10)**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is detecting property accumulation anomalies by procurement contractors — identifying entities that hold disproportionate property assets relative to their contract income, a pattern consistent with money laundering and Phoenix company financial cycling. The processing is necessary for this purpose because CCOD is the only authoritative bulk source of UK corporate property ownership data, and detecting accumulation anomalies requires cross-referencing property ownership against procurement award values. The data subjects' interests are not overridden because: (a) CCOD is a statutory public register under the Land Registration Act 2002, published specifically to enable transparency of land ownership; (b) processing is limited to corporate ownership fields (company_number, title_number, price_paid, tenure) — personal-name Proprietor fields are received but not stored in the graph; and (c) the legitimate public interest in detecting procurement corruption via property ownership patterns clearly overrides any residual privacy interest in publicly registered land ownership data.

### Licence

Open Government Licence v3.0 (OGL v3). Attribution required: "Contains HM Land Registry data © Crown copyright and database right 2024. This data is licensed under the Open Government Licence v3.0." Data available at: https://use-land-property-data.service.gov.uk/datasets/ccod

### Personal Data Fields Processed

| Field | Source | Use | Persisted? |
|-------|--------|-----|-----------|
| `Company Registration No. (1–4)` | CCOD bulk CSV column | Links CCOD record to Companies House entity in graph | Yes — stored as `company_number` on `PropertyData` node |
| `Title Number` | CCOD bulk CSV | Unique property identifier | Yes — stored as `title_number` on `PropertyData` node |
| `Tenure` | CCOD bulk CSV | Freehold/leasehold classification | Yes — stored on `PropertyData` node |
| `Price Paid` | CCOD bulk CSV | Accumulation ratio calculation | Yes — stored as `price_paid` on `OwnsProperty` edge |
| `Proprietor Name (1–4)` | CCOD bulk CSV | **Not processed** — personal data where natural person appears | No — field is skipped; only company_number is used for graph linking |
| `Date of Proprietorship (1–4)` | CCOD bulk CSV | Start of the ownership period — carried onto the ownership edge so ownership is temporally bounded | **Yes** — parsed into `CcodRecord.date_proprietor_added` (`land_registry_ccod.rs:53,294`) and written as `effective_from` on the `OwnsProperty` edge (`:575`). OCOD does the same (`land_registry_ocod.rs:727`) |

Note: CCOD primarily covers UK companies as proprietors. Personal data (natural person proprietor names) arises in a minority of records (LLP members, sole trader vehicles). These fields are explicitly not stored — the adapter skips the Proprietor Name columns and keys exclusively on company_number.

### Data Minimisation

Only company_number, title_number, tenure, price_paid and the date of proprietorship are stored. Proprietor Name columns are not modelled in the CCOD Rust data structures and are ignored during CSV parsing. No individual proprietor names, addresses, or dates of birth are retained.

> **Corrected 2026-08-23.** The field table previously recorded "Date of Proprietorship — Persisted? No". It is persisted, as the `effective_from` bound on the ownership edge. It is a date attached to a corporate ownership record rather than a personal identifier, so the minimisation position is unchanged — but the document must not claim a field is discarded when it is stored.

### Retention

`OwnsProperty` edge records and associated `PropertyData` nodes are retained for 5 years following the expiry or termination of the associated procurement contract, after which the `price_paid` field is zeroed (pseudonymised). Title numbers and company numbers (non-personal data) may be retained beyond this period as static identifiers.

### DPIA Assessment

Low risk. CCOD data is a bulk statutory public register; processing is limited to corporate ownership fields; no personal data (proprietor names) is stored. No DPIA required under UK GDPR Article 35 threshold assessment.

---

## Land Registry — OCOD Bulk CSV (Overseas Ownership) + Register of Overseas Entities (ROE)

**Status: Complete and licensed (Phase 22, PROP-05/PROP-06, OPS-01).** The HMLR licence covering both OCOD and CCOD was signed by the operator on 2026-06-17, and OCOD has been ingested and validated live (~97.5k records). The adapter remains **inert unless keyed**: `config.toml` ships `[enricher.ocod] api_key = ""`, and an absent or empty key means `is_enabled() == false`. The live key is supplied out of band via `URBAN__ENRICHER__OCOD__API_KEY` (or inherited from the shared `[enricher.land_registry]` key — one HMLR account licenses both datasets) and is never committed.

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is detecting overseas-owned property held by procurement contractors and surfacing potential offshore-ownership concealment, a pattern consistent with money laundering, sanctions evasion, and beneficial-ownership obfuscation in public procurement. Processing is necessary because OCOD is the only authoritative bulk source of overseas-company ownership of UK property, and the Register of Overseas Entities (ROE) is the statutory provenance for overseas-entity identifiers. The data subjects' interests are not overridden because: (a) OCOD is a statutory public register under the Land Registration Act 2002, and ROE is a statutory transparency register established by the Economic Crime (Transparency and Enforcement) Act 2022, both published specifically to enable transparency of overseas ownership of UK land; (b) processing is keyed on corporate identifiers (company number or `OE######` ROE number) and country of incorporation; and (c) the legitimate public interest in detecting procurement corruption and offshore concealment clearly overrides any residual privacy interest in publicly registered overseas land ownership.

### Licence

Open Government Licence v3.0 (OGL v3). Attribution required: "Contains HM Land Registry data © Crown copyright and database right 2024. This data is licensed under the Open Government Licence v3.0." Data available at: https://use-land-property-data.service.gov.uk/datasets/ocod

### Personal Data Fields Processed

| Field | Source | Purpose | Stored? |
|-------|--------|---------|---------|
| `Company Registration No. (1–4)` | OCOD bulk CSV | Owner node key — ROE `GB-ROE:OE######`, otherwise `GB-OCOD:` (bare number where genuine, else country-keyed). **Never `GB-COH:`** — keying an overseas company into the UK namespace would falsely attribute its property to an unrelated UK company (`land_registry_ocod.rs:83-84,194`) | Yes — `company_number` on `PropertyData` node |
| `Country Incorporated (1–4)` | OCOD bulk CSV | Canonicalised (`canonicalise_country`) overseas jurisdiction | Yes — `country_incorporated` on `PropertyData` node |
| `Title Number` | OCOD bulk CSV | Unique property identifier | Yes — `title_number` on `PropertyData` node |
| `Tenure` / `Price Paid` | OCOD bulk CSV | Property classification + accumulation ratio | Yes — `PropertyData` node / `OwnsProperty` edge |
| `Proprietor Name (1–4)` | OCOD bulk CSV | Owner display name, and the key for the exact-name OCOD↔ICIJ match | **Yes — permanently, on a graph node.** Every proprietor becomes an `OffshoreEntity` node whose `name` is the proprietor name (`land_registry_ocod.rs:685-697`), projected into the `offshore_entity` table (`crates/ingestor/src/projector.rs:956-961`). Where the row carries no company number the name also derives the node id |

Note: OCOD proprietors are overseas **companies**, so proprietor names are corporate (not personal) in the overwhelming majority of records.

> **Corrected 2026-08-23 — this is the most consequential error found in this document.** The previous text said the proprietor name "is not stored on the property node and appears only in the evidence of an analyst-gated `pending_review` link", and that a natural-person proprietor is protected by "the analyst confirm gate". Neither holds. The name is written **unconditionally** to a permanent `OffshoreEntity` graph node with an `OwnsProperty` edge, on every OCOD row, before any ICIJ matching is considered. The analyst gate governs the ICIJ *cross-reference*, not the ownership assertion.
>
> The narrow claim that survives is that the name is not on the **property** node — it is on the **owner** node. Where an overseas proprietor is a natural person, their name and their UK property ownership are recorded automatically, with no gate.

### Data Minimisation

Property nodes store only corporate identifiers, country of incorporation, tenure, and price. The proprietor name is **not** transient: it is persisted on the `OffshoreEntity` owner node, as recorded in the table above. What remains true is that the name is never used to create an *auto-linked* relationship — the ICIJ cross-reference is capped at 0.65 and gated at `pending_review`. No dates of birth or addresses are retained.

### ICIJ Isolation (ODbL ↔ OGL boundary)

OCOD (OGL v3) and the ICIJ Offshore Leaks tables (ODbL) are never merged. The OCOD↔ICIJ matcher reads the isolated `icij_offshore_entity` table read-only and writes a single **analyst-gated reference** `identity_link` at confidence 0.65, status `pending_review` — never auto-linked. This is a cross-reference pointer for analyst review, not a data merge or a published derived dataset, preserving the licence boundary (ENR-10, T-22-03-07). The link becomes a traversable graph assertion only after an analyst explicitly confirms it.

### Retention

`OwnsProperty` edges, `PropertyData` nodes, and OCOD↔ICIJ `identity_link`s follow the CCOD retention policy: retained for 5 years after the associated procurement contract expires/terminates, after which `price_paid` is zeroed. Unconfirmed `pending_review` OCOD↔ICIJ links are purged on review rejection.

### DPIA Assessment

> ⚠ **This assessment rests on a premise shown false on 2026-08-23 and REQUIRES OPERATOR RE-ASSESSMENT.** It concluded that personal-data exposure "is confined to analyst-gated pending-link evidence and never auto-linked". Proprietor names are in fact persisted unconditionally on permanent `OffshoreEntity` nodes (see the field table above). The conclusion below is retained verbatim for audit, but must not be relied on until re-assessed against what the adapter actually stores. Re-assessment should establish how many of the ~29.9k live owner nodes name natural persons rather than companies — the previous text deferred exactly that question ("subject to review if OCOD volume reveals material natural-person proprietor counts") without it ever being answered.

**Superseded assessment (retained for audit):** Low-to-medium risk. OCOD/ROE are bulk statutory public registers; processing keys on corporate identifiers; the only personal-data exposure (proprietor names of rare natural-person overseas proprietors) is confined to analyst-gated pending-link evidence and never auto-linked. The deliberate 0.65 cap + `pending_review` gate on offshore attribution is the primary safeguard. No formal DPIA required under the UK GDPR Article 35 threshold, subject to review if OCOD volume reveals material natural-person proprietor counts.

---

## London Gazette — Insolvency Notices

**Status: Complete (Phase 18, ENR-10)**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is providing earlier insolvency signals for procurement contractors than Companies House streaming can provide — the Gazette publishes winding-up notices, voluntary liquidation notices, and administration orders under the Insolvency Act 1986 before they appear in CH data, enabling earlier Phoenix Loop detection. The processing is necessary for this purpose because the London Gazette is the statutory publication vehicle for corporate insolvency notices under the Insolvency Rules 2016, and there is no alternative source providing the same near-real-time statutory insolvency data. The data subjects' interests are not overridden because: (a) Gazette notices are statutory public records under the Insolvency Act 1986 and Insolvency Rules 2016 — publication is legally mandated; (b) only the company CH number is extracted — the notice body text (which may mention insolvency practitioners) is not stored; and (c) the legitimate public interest in detecting imminent Phoenix company activity clearly overrides any residual privacy interest in an entity's published insolvency status.

### Licence

Open Government Licence v3.0 (OGL v3, Crown Copyright). Attribution required: "London Gazette notice data reproduced under the Open Government Licence v3.0. © Crown copyright." Data available at: https://www.thegazette.co.uk/data

### Personal Data Fields Processed

| Field | Source | Use | Persisted? |
|-------|--------|-----|-----------|
| `companyNumber` (CH number) | Gazette notice JSON-LD | Links insolvency notice to graph entity | Yes — used as entity_id lookup key |
| `publicationDate` | Gazette notice metadata | — | **No — never read.** The string `publicationDate` does not appear anywhere in `london_gazette.rs`; only `companyNumber` is extracted. The `NodeUpdated` event carries the enrichment run's wall clock, not the notice date |
| Insolvency practitioner name | Gazette notice body text | **Not processed** — name may appear in notice text but is not extracted | No — notice body is not parsed |

Note: The Gazette adapter extracts only the company CH number from each notice. The notice body text (which may contain insolvency practitioner names and addresses) is not parsed or stored.

> **Corrected 2026-08-23.** Three places in this section claimed the notice publication date is extracted and persisted. It is not read at all. This *reduces* the data processed relative to what was documented, so the minimisation and risk positions below are unaffected — but it also means the InLiquidation timestamp reflects when the enricher ran, not when the notice was published, which matters for any downstream reasoning about insolvency timing (Phoenix windows in particular).

### Data Minimisation

Only the company CH number (non-personal data) is extracted and stored. No personal data fields are retained. The `EntityStatus::InLiquidation` flag update contains no personal data.

### Retention

`NodeUpdated` graph events recording InLiquidation status are retained for 5 years following the expiry or termination of associated procurement contracts, consistent with the standard graph data retention policy.

### DPIA Assessment

Low risk. Only corporate identifier data (CH number) is processed; no personal data is stored. Processing is of statutory public record data. No DPIA required.

---

## Charity Commission — Register of Charities

**Status: Complete (Phase 19, FUZ-02, ENR-10). Completed retrospectively 2026-07-26 against the shipped adapter.**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is identifying charity–contractor overlaps in public funding, where a procurement contractor is also a registered charity or shares corporate identity with one, revealing conflict-of-interest and funding-recycling patterns. The processing is necessary because the Register of Charities is the only authoritative public source linking charity numbers to company numbers.

**As implemented, this adapter processes no personal data at all.** The interests of data subjects are therefore not engaged: the shipped adapter joins **exclusively on the non-personal company registration number** and never consults trustee names. The lawful basis is recorded here for completeness and in case a corroborated personal-data path is ever reinstated.

### Licence

Open Government Licence v3 (OGL v3). Bulk data is published as the `publicextract.*` dataset on Azure blob storage — **not** the older BCP-schema download (the adapter was migrated to the Azure schema on 2026-06-09, commit `266f085`). Register landing page: https://register-of-charities.charitycommission.gov.uk/register/full-register-download

### Personal Data Fields Processed

**None.** The fields actually consumed (`CharityRecord` in `crates/enricher/src/adapters/charity_commission.rs`) are:

| Field | Source | Use | Persisted? |
|-------|--------|-----|-----------|
| `charity_number` | Register bulk extract | Charity node key (`charity:<number>`) | Yes — `CharityData.charity_number` |
| `company_number` | Register bulk extract | **The sole match key** — exact join against `entity.company_number` | Yes — in `evidence_json` |
| `name` (charity name) | Register bulk extract | Charity display name (organisation, not personal data) | Yes — `CharityData.name` |

The resulting `identity_link` has `resolution_type = "exact_company_number"`, `confidence = 1.0`, `status = AutoLinked`, `source = "charity_commission"`, and an `evidence_json` containing only `charity_number`, `company_number`, `match`, and `score`. A `TrusteeOf` edge is emitted **entity → charity** — despite the edge name it asserts a corporate relationship derived from the shared company number, not an individual trusteeship.

**Trustee names are not read.** The trustee file in the bulk extract is explicitly excluded at the file-selection stage (`!name.contains("trustee")`), so trustee personal data never enters the process. The original stub anticipated processing trustee names, registration dates, and role natures; none of those are consumed.

### Why the trustee path was removed

The former name-only `fuzzy_trustee_name` resolver auto-linked **~87,000 false positives** at the ≥0.85 threshold (prefix bias plus single-word phonetic matching, with no corroborating identifier). Those links were purged on 2026-06-11 and the path was deleted under the exact-match-only policy (2026-06-13). Only the 224 `exact_company_number` links were ever trustworthy. This is a data-integrity control first, but it is also the strongest available data-minimisation measure: the adapter cannot mis-attribute a charitable trusteeship to a named individual because it never handles individual names.

### Data Minimisation

Maximal — no personal data is received into the Rust data structures, matched, or stored. Only charity number, company number, and charity name are retained.

### Retention

`Charity` nodes, `TrusteeOf` edges, and `identity_links` rows with `source='charity_commission'` contain no personal data and are retained indefinitely as corporate reference data. No pseudonymisation obligation arises.

### DPIA Assessment

No risk to data subjects. Processing is limited to a statutory public register's corporate identifiers, with no personal data received or stored. No DPIA required under the UK GDPR Article 35 threshold.

---

## OpenSanctions — PEP and Sanctions Screening

**Status: Complete (Phase 19, FUZ-03, ENR-10). Completed retrospectively 2026-07-26 against the shipped adapter.**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is screening procurement participants against sanctions designations and politically-exposed-person registers, so that a public body does not award a contract to a designated person or an undisclosed PEP. The processing is necessary because sanctions and PEP status cannot be derived from UK company data alone; it requires matching against consolidated designation lists.

The data subjects' interests are not overridden because: (a) sanctions designations are published by designating authorities precisely to make them public and enforceable, and PEP status derives from the holding of public office; (b) matching is performed against a **self-hosted** matcher, so no personal data is disclosed to a third party (see below); (c) a name-only match can never be auto-linked (the 0.65 cap, below), so no adverse designation is ever automatically attributed on the strength of a name alone; and (d) the sanctions/PEP identifiers, not the underlying personal attributes, are what get persisted.

**Special category and criminal-offence data.** A sanctions designation may relate to alleged criminal conduct, and PEP status may reveal political opinions. Because the source data is manifestly made public by the designating authority and by electoral/office-holding disclosure, Article 9(2)(e) is relied on for any political-opinion inference. The screening result is treated as a **review candidate for a human analyst**, never as an adjudication.

### Licence and config gate

Creative Commons BY-NC 4.0 (non-commercial use only). The licence requires `non_commercial_confirmed = true` in the enricher config (FUZ-03, D-06). This is a **fail-hard gate, not a warning**: with `enabled = true` and `non_commercial_confirmed = false`, `run()` returns `Err` *before any database or network access*, and `is_enabled()` returns false. Data available at: https://www.opensanctions.org/docs/bulk/

### No third-party disclosure

Matching runs against a **self-hosted `yente` sidecar** on `http://localhost:8766` (D-05). Candidate names — and, where present, birth date and nationality — are sent to that local process only. No personal data from the UrbanIntegrity graph is transmitted to OpenSanctions or any other external service, so no international transfer or processor arrangement arises.

### Personal Data Fields Processed

| Field | Source | Use | Persisted? |
|-------|--------|-----|-----------|
| `name` | Graph `EntityCandidate` (director/officer node) | Sent to the local yente matcher as `properties.name` | No — not written to the link record |
| `date_of_birth` | Graph candidate, when known | Sent as `properties.birthDate` when present; its **presence** lifts the 0.65 name-only cap | No — deliberately excluded from `evidence_json` (T-19-03-04) |
| `nationality` | Graph candidate, when known | Sent as `properties.nationality` when present; same cap effect | No — deliberately excluded from `evidence_json` (T-19-03-04) |
| `yente_id` (matched designation id) | yente response | Target node key (`sanctions:<yente_id>`) — the analyst's pointer to the designation | Yes — `evidence_json.yente_id` |
| `score` | yente response | Confidence, clamped to `[0.0, 1.0]` from **both** ends before use (T-19-03-01) | Yes — as `yente_score` and `translated_confidence` |

The stored `identity_link` has `resolution_type = "sanctions_screening"` and `source = "opensanctions"`. Its `evidence_json` contains only `yente_id`, `yente_score`, `translated_confidence`, a `name_only` boolean, and `source` — **no name, no date of birth, no nationality**. Designating authority and designation date are not extracted; the `yente_id` is the reference an analyst follows to the authoritative record.

### Name-only cap (D-08)

Where the graph candidate has neither a date of birth nor a nationality, the match is structurally uncorroborated and the confidence is capped at **0.65** regardless of the score yente returns (T-19-03-03). The routing ladder is `AUTO_LINK_THRESHOLD = 0.85`, `CANDIDATE_THRESHOLD = 0.70`, `SUB_CANDIDATE_THRESHOLD = 0.50` (`crates/enricher/src/confidence.rs`), so a capped name-only match cannot reach auto-link **or even `PendingReview`** — 0.65 falls below 0.70 and lands in `SubCandidate` at best, the weakest analyst-review tier. Anything under 0.50 is discarded outright. This is the primary safeguard against falsely attributing a sanctions designation to a UK director who merely shares a name with a designated person.

### Data Minimisation

Personal attributes are used transiently as match inputs to a local process and are never persisted. Only the designation identifier and the numeric scores are stored. The candidate's own graph node is pre-existing UK company-register data, unchanged by this adapter.

### Retention

`identity_links` rows with `source='opensanctions'` are retained for 5 years following the expiry or termination of the associated procurement contract. Because `evidence_json` holds no personal data, the standard pseudonymisation step is a no-op for this source; expiry means deletion of the link, not redaction of a field. Rejected links are permanently suppressed on rejection (REV-04). See "Retention Enforcement Status" above — this is policy, not yet automated.

### Data Subject Rights

- **Right to object (Article 21):** applies. A `pending_review` or `sub_candidate` screening hit is not acted on until an analyst confirms it, and `links reject` permanently suppresses it.
- **Right to erasure (Article 17):** the underlying designation is a public act of a designating authority; erasure requests belong to that authority. The derived link in this system can be rejected and suppressed.
- **Right of access (Article 15):** the matched designation is publicly accessible at OpenSanctions via the stored `yente_id`.

### DPIA Assessment

**Medium risk — the highest of any source in this document**, because a false positive amounts to a false imputation of sanctions or PEP status. Mitigations: the 0.65 name-only cap blocking auto-link, the analyst confirm gate, exclusion of DOB/nationality from persistence, both-ends score clamping, self-hosted matching with no third-party disclosure, and the fail-hard licence gate. A formal DPIA should be completed before this adapter is used to inform any decision affecting an identified individual, rather than solely to queue candidates for analyst review.

---

## ICIJ Offshore Leaks

**Status: Complete (Phase 19, FUZ-04, ENR-10). Completed retrospectively 2026-07-26 against the shipped adapter, following the write-path fix in PR #18.**

### Legal Basis

UK GDPR Article 6(1)(f) — Legitimate Interests. The specific legitimate interest is giving analysts a searchable offshore-entity reference alongside the procurement graph, so that a contractor's connection to an offshore structure can be investigated by a human. The processing is necessary because no UK public register discloses offshore beneficial-ownership structures at this depth.

This source carries a materially weaker balance than the statutory registers, and the design reflects that: **the data is held as an isolated analyst-reference corpus and is never joined to the graph automatically.** Zero identity links are created (see below). The interests of data subjects are protected by that isolation rather than by the publication status of the source, since ICIJ data derives from leaks and includes individuals never accused of wrongdoing.

### Licence and isolation

Open Database Licence (ODbL 1.0, share-alike). ICIJ rows are stored exclusively under the `icij_*` table prefix (`icij_offshore_entity`, `icij_offshore_link`) to maintain licence-required isolation from OGL-licensed sources (FUZ-04) — the ODbL share-alike obligation must not propagate into OGL-derived data. Data available at: https://offshoreleaks.icij.org/pages/database

The only permitted cross-boundary reference is the OCOD↔ICIJ matcher (PROP-06), which reads `icij_offshore_entity` **read-only** and writes a single analyst-gated link at confidence 0.65 — never an auto-link, never a merge. Note that 0.65 sits below `CANDIDATE_THRESHOLD` (0.70), so its status is **forced** to `PendingReview` as a deliberate documented deviation from the threshold ladder (`land_registry_ocod.rs`): the offshore-attribution class is defamation-grade, so it must reach an analyst rather than being routed to the weaker `SubCandidate` tier or discarded. See the OCOD section above, and the mandatory negative test asserting that a `pending_review`@0.65 link produces zero offshore-shadow detector candidates (`crates/enricher/src/offshore_shadow.rs`).

### Personal Data Fields Processed

A one-shot idempotent load from `full-oldb.LATEST.zip`. Two of the parsed record types are written; **officer records are parsed and then discarded**.

| Field | Record type | Stored as | Notes |
|-------|-------------|-----------|-------|
| `node_id` | entity / relationship | `node_id` | ICIJ's own identifier |
| `name` | entity | `name` | Corporate names — the offshore entity |
| `source_id` (`sourceID`) | entity / relationship | `dataset` (**required** string) | Which leak the row came from (Panama, Paradise, …) |
| `jurisdiction`, `country_codes`, `incorporation_date`, `status` | entity | same names | Corporate attributes |
| `node_id_start` / `node_id_end` | relationship | `from_node_id` / `to_node_id` | Edge endpoints inside the isolated tables |
| `rel_type` | relationship | `rel_type` | e.g. officer-of |
| **officer rows** — `node_id`, `name`, `country_codes` | officer | **nothing** | Parsed from `nodes-officers.csv` into `IcijOfficerRecord` and counted in the run log, but there is **no insert path for `load_result.officers`** — only `entity_row` → `icij_offshore_entity` and `link_row` → `icij_offshore_link` are written (`icij.rs:290-337`) |

Rows with an empty `node_id` or `name` are skipped with a WARN (T-19-04-03, ASVS V5). Registered addresses and filing dates — anticipated by the original stub — are **not** loaded by the shipped adapter.

> **Corrected 2026-08-23.** The table previously listed officer rows as persisted, including `name` annotated "For officers this is a **natural person's name**". No officer row has ever been written: the adapter parses them, logs the count, and drops them on the floor. `IcijOfficerRecord` has no `source_id` field, so it could not satisfy the schema's required `dataset` column even if an insert existed.
>
> For this document the correction is favourable — the single largest concentration of natural-person data in the ICIJ corpus is not in the database. **But it is also a probable defect of exactly the class PR #18 was raised to close** (see *Write-path integrity* below): a silently discarded half of the corpus. Whether officers should be loaded is a product decision; if they ever are, this section and the DPIA assessment must be revisited **before** the load, since it would introduce bulk natural-person data from a leaked-document source.

### No automatic linking — zero identity links

The adapter creates **no `identity_links` at all**: it returns `auto_linked: 0, pending_review: 0`. The former `fuzzy_name_match_offshore` resolver (Jaro-Winkler + Double Metaphone on surnames) was deleted under the exact-match-only policy. The reasoning is recorded in the code and is the load-bearing safeguard for this source: ICIJ records carry **no corroborating identifier** that can be joined to a UK director — no UK company number, no date of birth, no address overlap with the graph. Every such match was therefore structurally name-only, the exact class that produced ~87k false positives in the charity resolver and 3,513 in the Electoral Commission individual path. Attributing offshore involvement to a named individual on a name-only match is a defamation-grade error, so the automatic path does not exist. Should a corroborating exact identifier become available, an exact join could be reinstated.

### Write-path integrity (PR #18)

Until 2026-07, the ICIJ adapter **wrote zero rows for its entire operational life** while logging success: the row shape omitted the required `dataset` field and used `node_id_start`/`node_id_end` where the schema expects `from_node_id`/`to_node_id`, and every SurrealDB write `Result` was discarded via `let _ =`. PR #18 made the writes schema-conformant, batched (`INSERT IGNORE`, idempotent re-entry on the `node_id` index), and **fail-loud**. This is recorded here because it bears directly on any accuracy claim about this table: rows described above have only actually been present since that fix.

### Data Minimisation

The corpus is loaded whole — this is a reference dataset, not a targeted extraction, so field-level minimisation is limited to the columns listed above (addresses and filing dates are excluded). Minimisation is achieved structurally instead: the data lives in isolated tables, is never merged into the graph, produces no automatic assertions about any individual, and reaches an analyst's attention only via an explicitly confirmed, bounded reference link.

### Retention

`icij_offshore_entity` and `icij_offshore_link` rows are retained as a reference corpus for as long as the ICIJ release remains current; they are not aged out per row. Note that the load is **one-shot and skip-on-populated** (D-10): `run()` counts `icij_offshore_entity` first and skips entirely if any rows exist, so re-running does not refresh the corpus. Moving to a newer ICIJ release therefore requires an explicit operator step to drop the `icij_*` tables — which is also the erasure path below. Any OCOD↔ICIJ `pending_review` link is purged on review rejection. Because no link asserts a relationship to an identified UK individual, there is no per-subject retention obligation attaching to the graph side. See "Retention Enforcement Status" above.

### Data Subject Rights

- **Right to object (Article 21):** the strongest protection here is structural — no automated assertion about any individual is made, so there is no adverse processing outcome to object to. Any confirmed OCOD↔ICIJ reference link can be rejected and permanently suppressed (REV-04).
- **Right to erasure (Article 17):** requests concerning the underlying leak data are for ICIJ as publisher. A local copy can be dropped by deleting the `icij_*` tables, which does not affect any other source thanks to the isolation boundary.

### DPIA Assessment

**Medium risk, mitigated by isolation.** The source is leak-derived, includes natural persons, and is not a statutory public register — the highest-sensitivity input in the pipeline. Mitigations: complete absence of automatic linking, `icij_*` table isolation preserving the ODbL/OGL boundary, the 0.65 `pending_review` cap plus analyst confirm gate on the single permitted cross-reference, the mandatory negative test proving that link cannot fire a detector, and skip-with-WARN on incomplete rows. A formal DPIA should be completed before any ICIJ-derived link is used to inform a decision about an identified individual.

---

## Electoral Commission — Donations Register

### Legal Basis

**Primary basis:** UK GDPR Article 6(1)(e) — Public task. The Electoral Commission publishes donation records under a statutory obligation (Political Parties, Elections and Referendums Act 2000, s.62). Processing those records to detect financial relationships between political donors and public procurement decision-makers is directly ancillary to the public anti-corruption accountability function. The processing is necessary and proportionate: company registration numbers and donation amounts are publicly filed; individual donor names are also publicly published by the Electoral Commission under statutory disclosure requirements.

**Secondary basis:** UK GDPR Article 6(1)(f) — Legitimate interests. Detecting potential quid pro quo corruption (company donates to controlling party → company receives contract from party-controlled LPA within 730 days) serves the legitimate anti-corruption accountability interest of transparency campaigners, journalists, and the public. This interest overrides any privacy interest because the data is already publicly disclosed under electoral law.

**Special category data (Article 9):** To the extent that political party affiliation may be inferred from donation data (e.g., an individual donor's name appears in association with a specific party), Article 9(2)(e) applies — this is manifestly public data published under UK electoral law, making processing lawful without further condition.

**Source statutory authority:** Political Parties, Elections and Referendums Act 2000 (PPERA 2000); Electoral Commission Transparency Register.

### Licence

Open Government Licence v3 (OGL v3). Free reuse with attribution. No restriction on analytical or research use. Attribution: "Electoral Commission Political Finance Register — data.electoralcommission.org.uk". Data available at: https://www.electoralcommission.org.uk/who-we-are-and-what-we-do/financial-reporting/donations-and-loans

### Personal Data Fields Processed

| Field | Category | Processing Purpose | Storage in System |
|-------|----------|--------------------|-------------------|
| `DonorName` (individual donors) | Personal data — full name | **Not processed for matching.** Individual-donor identity matching is disabled (exact-match-only policy, 2026-06-13): an individual donation carries no corroborating identifier, so the name is read during CSV parse and discarded — no identity link is created. | Not stored. No slug is derived and nothing is written to identity_links for individual donors. |
| `CompanyRegistrationNumber` | Not personal data — company identifier | Exact join against entity.company_number (confidence=1.0) | Stored in evidence_json as part of the identity_link record |
| `Value` | Not personal data — financial amount | Threshold filtering; correlation evidence | Stored in evidence_json |
| `AcceptedDate` | Not personal data — transaction date | 730-day correlation window calculation | Stored in evidence_json as effective_from on DonatesTo edge |
| `RegulatedEntityName` | Not personal data — party/organisation name | Party name lookup in party_authority_control.json | Stored in evidence_json |
| `DonorStatus` | Not personal data — donor classification | Routes company donors to an exact `company_number` join; individual donors are discarded (no identity matching) | Not stored separately |

**Privacy-preserving design:** Under the exact-match-only policy (2026-06-13), individual donor names are not matched at all — the individual-donor path is disabled, so an individual donor name is read transiently during CSV parse and discarded without deriving a slug or writing any record. Only company donors (matched on the non-personal `company_number`) produce identity_links. The net effect strengthens Article 5(1)(e) storage limitation: no individual-donor personal data is retained in any form.

### Data Subject Rights

- **Right to object (Article 21):** Largely moot for the Electoral Commission source since the individual-donor matching path is disabled — no identity_links are created from individual donor names. For any company-donor links (keyed on the non-personal `company_number`), analysts can still reject a link via the `links reject` CLI command; rejected links are permanently suppressed (REV-04), per-link.
- **Right to erasure (Article 17):** Does not apply to publicly published statutory registers. The Electoral Commission is the data controller for the original publication; UrbanIntegrity processes a derivative of public-domain data for anti-corruption research. No erasure right arises under Article 17(3)(d) (processing necessary for archiving in the public interest).
- **Right of access (Article 15):** Fulfilled via the Electoral Commission's own publication — all data processed by UrbanIntegrity is already publicly accessible at source.

### Retention

5 years from date of enrichment, aligned with the public procurement cycle retention period (standard government contract audit window). Note: with the individual-donor matching path disabled (2026-06-13), no individual-donor personal data is stored, so the former "pseudonymise donor name slugs after 5 years" provision no longer has any data to act on; only company-donor links (no personal data) and aggregate statistics remain, the latter retained indefinitely.

---

## Council Registers of Interest

### Legal Basis

**Primary basis:** UK GDPR Article 6(1)(e) — Public task. Council members are required to register their interests under the Localism Act 2011, s.29, and the Local Authorities (Model Code of Conduct) Order 2001. The Local Government Transparency Code 2015 requires publication of those registers. Processing the published registers to identify potential undisclosed financial conflicts of interest in procurement decisions is directly ancillary to the statutory transparency and anti-corruption accountability function.

**Processing rationale:** Councillors are public officials exercising public authority. The UK GDPR Recital 97 expressly acknowledges reduced privacy expectations for personal data relating to the public roles of public officials. The processing is limited to declared interests in their capacity as public officials, not to their private lives.

**Source statutory authority:** Localism Act 2011 (s.29); Local Authorities (Model Code of Conduct) Order 2001; Local Government Transparency Code 2015.

### Licence

Data is published under the Local Government Transparency Code 2015, which compels publication as a matter of law. Each council's register is published on its own website; typical publication terms are implicitly Open Government Licence v3 or equivalent open licence (consistent with the statutory duty to publish). The processing does not require any licence grant from individual councils because the data is publicly compelled statutory disclosure.

### Personal Data Fields Processed

| Field | Category | Processing Purpose | Storage in System |
|-------|----------|--------------------|-------------------|
| Councillor name | Personal data — public official | **Not a match key.** Lower-cased and hyphenated only to derive the node id (`political_actor:{slug}`) | **Yes — the RAW name.** `PoliticalActorData.name` is `decl.councillor_name` verbatim (`council_registers.rs:322`), written to `graph_events` and projected into `political_actor` |
| Declared company name | Not personal data in isolation — company name | **This is the only value compared.** Lower-cased and looked up against a name→entity map (`council_registers.rs:305-306`) | Stored in evidence_json |
| Nature of interest (financial/non-financial) | Personal data — financial interest classification | Evidence type indicator in identity_link record | Stored in evidence_json |
| Council name | Not personal data — organisation | Identifies the LPA for PoliticalActor node creation | Populated with the **LPA code**, not a council name (`council_registers.rs:324`). ~~Dropped by the projector~~ — **that was fixed 2026-08-26 (`a01f4d0`)**: `projector.rs:920-930` now writes `party` and `council_name` as well as `entity_id` and `name`, guarded by `tests/projector_schema_parity.rs`. The LPA code DOES now reach the read model |

> **Corrected 2026-08-23 — three claims in the table above were wrong.**
>
> 1. **The councillor name is not the match key**, and no name-equality matching against director slugs occurs. The only comparison is the declared *company* name (`decl.company_name.to_lowercase()`) against a lookup map. The earlier table described this as matching on the councillor name, which was wrong. (Narrowed 2026-09-03: the literals `resolution_type: "councillor_exact_name"` and `confidence: 1.0` DO exist in the shipped code at `council_registers.rs:342-343` — this bullet previously said the description "did not correspond to the shipped code", which sends anyone who greps for those strings to the wrong conclusion. What is wrong is the *implication* that a councillor name was compared, not the existence of the label.)
> 2. **The raw councillor name IS persisted**, on a `PoliticalActor` graph node, not merely "as a normalised slug in identity_links". The slug is only the record id.
> 3. **`council_name` holds an LPA code.** ~~and is dropped by the projector in any case, so it never reaches the read model~~ — no longer true; the projector drop was fixed 2026-08-26 (`a01f4d0`), so the LPA code now does reach the read model. Retained here because the claim was accurate when written and the correction matters for the data-protection position.
>
> ⚠ **Two code defects this correction surfaced.** The first is **fixed** (2026-08-23): the lookup map was built from `SELECT id, name, entity_id FROM officer LIMIT 50000` — the **officer** table, which holds people — so declared *company* names were compared against *person* names, and the fixed `LIMIT` truncated a table holding 73,865 rows. It now reads the `entity` table, paged. The second is now also **fixed** (2026-08-23, `a611177`): the `Directs` edge this adapter emitted ran from a `political_actor` node, but the schema defines `directs TYPE RELATION FROM entity | officer TO entity | officer`, so SurrealDB rejected the relate outright (`Expected record<entity|officer> but found political_actor:…`, verified 2026-08-23) and no declared-interest edge had ever reached the graph. Declared interests now have their own relation, `declares_interest TYPE RELATION FROM political_actor TO entity` — deliberately *not* a widened `directs`, because a self-reported disclosure must not be recorded as a Companies House directorship.
>
> Neither changes the data-protection position recorded above, which describes what the adapter stores.

**Personal addresses:** Not processed. The adapter targets only declared company interests, not residential or personal addresses.

**Party membership:** Not directly processed from registers. Party data is inferred from the separate party_authority_control.json mapping (Open Council Data UK source), not from individual councillor declarations.

### Data Subject Rights

- **Right to object (Article 21):** Applies. Councillors may challenge incorrect or disputed declarations via the `links reject` CLI. Rejected identity_links are permanently suppressed (REV-04).
- **Right to erasure (Article 17):** Does not apply. Statutory register data published under a legal obligation (Localism Act 2011) is exempt under Article 17(3)(b). Councillors seeking removal of declarations must apply to their council under the Model Code of Conduct appeals process.
- **Reduced privacy expectations:** UK GDPR Recital 97 — processing of personal data of public officials is subject to a proportionality assessment that acknowledges their reduced expectation of privacy in respect of their public duties.

### Retention

5 years from date of enrichment. After 5 years: councillor name identity_links are pseudonymised (slug replaced with SHA-256 hash). Aggregate statistics (number of councillors matched per LPA per year) are retained indefinitely as they contain no personal data.

---

## OpenCorporates — Cross-Jurisdiction Companies

### Legal Basis

**Primary basis:** UK GDPR Article 6(1)(e) — Public task. Director names in company registers are publicly filed data under company law in every jurisdiction. Processing cross-jurisdiction corporate registrations to resolve entities used to obscure beneficial ownership in public procurement directly supports the anti-corruption accountability function.

**Processing rationale:** The processing resolves cross-jurisdiction corporate structures — specifically, identifying where a company registered in a foreign jurisdiction is the same entity as a company in the UK graph, or where a director appears in multiple registries under variant name forms. This is necessary to detect procurement fraud where beneficial ownership is obscured through offshore or foreign-registered shell companies.

**Source:** OpenCorporates aggregates public company registry data from 140+ jurisdictions, all of which mandate public filing of director names. No data is sourced from private databases.

### Licence

**Open Database Licence (ODbL).** OpenCorporates publishes its aggregated company data under ODbL. The ODbL share-alike obligation applies only to *published derivative databases* — it does not restrict internal analytical use. UrbanIntegrity's use is internal anti-corruption research that does not involve publishing a derived database; the share-alike obligation therefore does not apply.

**Config gate:** The `oc_open_data_confirmed = true` configuration flag must be explicitly set before the adapter activates. This gate ensures the analyst has acknowledged the ODbL terms (mirrors the OpenSanctions `non_commercial_confirmed` pattern, D-06). The flag prevents accidental activation without explicit licence acknowledgement.

**Monthly quota gate:** The free tier is limited to 200 API requests/month (configurable via `monthly_limit`). The quota counter is persisted in SurrealDB (`oc_quota` table) to prevent bulk extraction that would constitute a derivative database requiring ODbL relicensing.

**Data source:** https://opencorporates.com/info/our-data/

### Personal Data Fields Processed

| Field | Category | Processing Purpose | Storage in System |
|-------|----------|--------------------|-------------------|
| Director names (as filed per jurisdiction) | Personal data — full name as filed | **No longer processed for matching.** Cross-registry resolution now joins purely on exact `company_number` (resolution_type `exact_company_number`); director names are not consulted as a match key. | Not stored. The adapter writes only the matched company number, jurisdiction, and company name to evidence_json — no director personal data. |
| Jurisdiction of incorporation | Not personal data — corporate attribute | Cross-jurisdiction entity resolution | Stored in evidence_json |
| Registered address (company address) | Not personal data — company address (not residential) | — | **Not stored.** `evidence_json` holds exactly `oc_company_number`, `jurisdiction`, `oc_name` and `match` (`opencorporates.rs:328-335`) — no address field is written. Corrected 2026-08-23; the paragraph below already stated this, contradicting the row |
| company_number (per jurisdiction) | Not personal data — company identifier | Primary join key for cross-registry resolution | Stored in evidence_json and as part of identity_link |

**Privacy-preserving design:** Under the exact-match-only policy (2026-06-13), the OpenCorporates adapter links cross-registry records solely on an exact `company_number` match. Director names returned by the API are not used for matching and are not written to the database — evidence_json holds only the company number, jurisdiction, and company name. No director personal data is retained (Article 5(1)(e) storage limitation).

### Data Subject Rights

- **Right to object (Article 21):** Applies. Analysts can reject individual identity_links via `links reject` CLI. Rejected links are permanently suppressed (REV-04).
- **Right to erasure (Article 17):** Does not apply to publicly filed company register data. Director names are publicly disclosed under the company law of each filing jurisdiction; UrbanIntegrity processes a derivative of public-domain data for anti-corruption research (Article 17(3)(d) exemption).
- **Right to portability (Article 20):** Not applicable — processing is under Article 6(1)(e) public task, not consent or contract.

### Retention

5 years from date of enrichment. After 5 years: director name slugs in OpenCorporates identity_links are replaced with SHA-256 hashes (irreversible pseudonymisation). Company-level records (jurisdiction, registration numbers) are retained indefinitely as they contain no personal data.

---

*Skeleton created: Phase 16. Each section must be completed by the responsible phase before that adapter ships — see requirement ENR-10 (v3.0 requirements). Requirements are kept per milestone **and** in a top-level requirements file holding the milestone in progress. **RET-01–06 now live in the v6.0 requirements** — the 2026-08-23 correction pointed at the top-level file, which was true then but has since rolled over to v7.0, then v8.0, and now v9.0 (Fraud Analyst Workbench; still no RET). Re-corrected 2026-09-03.*

> **⚠ Open section — `Disqualification` node kind (added 2026-08-23, `14f8e11`).** This document's
> own merge gate requires a completed section naming concrete personal-data fields before an
> adapter ships. `Disqualification` shipped as its own node kind without one: `schema/src/db.rs:140-149`
> defines a `disqualification` table and `:187` retypes the relation to
> `disqualified_from TYPE RELATION FROM officer TO disqualification`. The CH-Disqualified section
> below still describes only the `DisqualifiedFrom` edge.
>
> **Factual position, for whoever completes the section:** the node carries `case_number` and two
> dates and no name or address, so the data-protection position looks unchanged from the edge it
> replaced — but the lawful-basis and retention statements are an operator judgement and are
> deliberately NOT asserted here. Recorded 2026-09-03 by a documentation audit, not by the phase
> that shipped the node; this is the fourth failure of the merge gate, not one of the three the
> document already records.

*Reviewed 2026-07-26: completed the three Phase 19 sections (Charity Commission, OpenSanctions, ICIJ) that had shipped as stubs; updated the OCOD status to reflect the signed HMLR licence and live validation; added the "Retention Enforcement Status" section recording that no retention automation exists. All statements were verified against the adapter sources in `crates/enricher/src/adapters/` and `config.toml`, not against the planning docs.*

---

## Adapters Without a Section

`crates/enricher/src/adapters/mod.rs` declares **14** adapters. This document covers **11**. The three below shipped without the section this document's own merge gate requires, and are recorded here as an explicit gap rather than left silently absent.

**These are factual data-flow records only. No lawful basis, licence position, retention period or DPIA conclusion has been assessed for any of them — those require the operator, and in the PPD case likely a DPIA before further processing.** Nothing here should be read as a compliance clearance.

### `land_registry_ppd` — Price Paid Data (Phase 23, FLIP-01)

**The most significant gap.** `PpdRecord` (`land_registry_ppd.rs:45-65`) parses and persists to `ppd_transactions`:

| Field | Note |
|-------|------|
| `paon`, `paon_norm`, `saon` | Primary/secondary addressable object — i.e. **house number or name, and flat/unit** |
| `street`, `street_norm`, `locality`, `town`, `district`, `county` | Full postal address components |
| `postcode_raw`, `postcode_norm` | Full postcode |
| `price_gbp`, `date_of_transfer`, `property_type`, `tenure`, `old_new`, `ppd_category` | Transaction attributes |
| `transaction_id`, `district_code` | Identifiers |

Taken together these are **full residential property addresses with transaction prices and dates**. PPD covers residential sales by private individuals, so unlike CCOD/OCOD the subject of a row is routinely a natural person's home, even though the register does not name the buyer or seller. An address plus a sale price and date is capable of identifying a household. No legal basis, licence note, minimisation statement, retention period or DPIA has been recorded for this source anywhere.

### `mutuals_register` — FCA Mutuals Public Register (Open Item 24)

Largely corporate (societies and their registration numbers), but persists registered addresses as `Location` nodes. A registered address for a small society can be a member's home address.

### `planning_identity` — Planning-Applicant Identity Resolution (IDN-05)

Matches planning-applicant **personal names** against Companies House officer names by exact normalised equality, and on a match writes **both** an `identity_links` audit row **and** a real `Directs` graph edge (`planning_identity.rs:5-9`). It is the one adapter in this codebase that asserts a directorship relationship into the graph from a name match, which makes its accuracy properties materially different from the reference-only sources documented above. Matches in the 0.70–0.85 band are written for analyst review and produce no graph edge (`:66-67`).

## Outstanding compliance work

1. **Automate retention.** No age-based pseudonymisation or deletion exists (see "Retention Enforcement Status"). The 5-year commitments in this document are currently unenforced.
2. **Formal DPIAs.** Two sources are assessed medium risk and carry a DPIA precondition before their output informs any decision about an identified individual: OpenSanctions (false imputation of sanctions/PEP status) and ICIJ (leak-derived personal data).
3. **Honour the merge gate.** The Phase 19 adapters shipped with stub sections; three further adapters (PPD, mutuals register, planning identity) shipped with no section at all. The gate is a documented rule with no enforcement mechanism, and it has now failed twice. Either enforce it in CI (fail when `adapters/mod.rs` declares a module with no matching section here) or stop describing it as a merge blocker.
4. **Complete the three missing sections** in *Adapters Without a Section*, starting with `land_registry_ppd` — it stores full residential addresses with prices and dates and has no recorded basis, licence position or retention period.
5. **Re-assess the OCOD DPIA.** Its "low-to-medium risk" conclusion rested on the belief that proprietor names are never persisted outside analyst-gated evidence. They are written unconditionally to permanent owner nodes. The open question the original assessment deferred — how many owner nodes name natural persons rather than companies — is now the blocking one.
6. **Extend retention scope beyond `identity_links`.** RET-01 is written in terms of `identity_links.evidence_json`, but personal data is also held in the clear on graph nodes: `Psc.name`, `PoliticalActor.name`, and `OffshoreEntity.name`. Pseudonymising only the link evidence would leave the node weights untouched.
