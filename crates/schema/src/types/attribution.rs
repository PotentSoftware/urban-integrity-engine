//! D1's attribution registry — a hand-maintained mapping from every OSINT evidence term this
//! phase's Tier-2 probes count to its real producer set (PRB-05/GRD-03/GRD-04, VRD-08).
//!
//! Pure in-process data: `&'static str` literals and function pointers only, no I/O, no
//! `surrealdb` dependency. `schema` does not and must not depend on `core` — the `Attributed`
//! wrapper that consumes this registry (Plan 33-03) lives in `crates/core` instead, layered on
//! top of `core::persistence::scan`'s existing `scan_events_by_type`/
//! `scan_events_by_payload_contains`.
//!
//! # Why `Directs` has no entry
//!
//! `Directs` is deliberately absent from [`REGISTRY`]. It has two producers:
//! `crates/enricher/src/adapters/planning_identity.rs:255-258` (inside this registry's declared
//! search space, `crates/enricher/src/adapters/**`) and `crates/ingestor/src/ch/mapper.rs:184`
//! (outside it). A registry entry naming only the in-scope producer would silently under-claim
//! `Directs`' real provenance, and no in-scope Tier-2 probe counts `Directs` as evidence for a
//! source this phase needs to attribute — so adding an entry that can never be verified by Plan
//! 33-03's producer-coverage test would be a claim this phase cannot back up. See this plan's
//! `<interfaces>` section for the full verification trail.

use serde_json::Value;

/// One evidence term this phase's probes count, and the real adapter(s) that produce it.
///
/// `producers` is the set of `Adapter::name()` strings (verified live against
/// `crates/enricher/src/adapters/*.rs`) whose emit sites construct this term. An empty slice is a
/// legitimate entry — `OffshoreLink` has zero real producers today, which is a fact this registry
/// exists to make checkable, not an omission to fill in later.
///
/// `discriminator` narrows a *single* term to a specific producer when more than one adapter
/// constructs the same `NodeWeight`/`EdgeWeight` variant with different field shapes (D2). `None`
/// means either there is exactly one producer, or no in-scope probe needs the narrower claim.
#[derive(Debug, Clone, Copy)]
pub struct EvidenceTerm {
    pub term: &'static str,
    pub producers: &'static [&'static str],
    pub discriminator: Option<fn(&Value) -> bool>,
}

/// The 13-entry attribution registry (D1). `Directs` is deliberately absent — see the module
/// doc comment above.
///
/// Producer names and emit sites verified live this session (2026-08-26) against
/// `crates/enricher/src/adapters/*.rs`; see this plan's `<interfaces>` section for the full
/// file:line trail behind each entry.
///
/// Deviation from Plan 33-02: the plan's frontmatter/acceptance-criteria state `REGISTRY.len()
/// == 12`, but its own `<action>` enumerates 13 distinct terms with full file:line verification
/// for each (PoliticalActor, DeclaresInterest, Property, Psc, Charity, OffshoreEntity,
/// Disqualification, DisqualifiedFrom, Controls, OffshoreLink, DonatesTo, TrusteeOf,
/// InLiquidation). The enumerated, evidence-backed list is treated as the operative spec; the
/// `12` count elsewhere in the plan is a miscount corrected here (Rule 1).
pub static REGISTRY: &[EvidenceTerm] = &[
    EvidenceTerm {
        term: "PoliticalActor",
        producers: &["council_registers", "electoral_commission"],
        discriminator: Some(is_council_registers_political_actor),
    },
    EvidenceTerm {
        term: "DeclaresInterest",
        producers: &["council_registers"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "Property",
        producers: &["land_registry_ccod", "land_registry_ocod"],
        // D3's aggregate-across-producers shape (Plan 33-07) never claims the count is FROM one
        // specific source, so it needs no discriminator here — `producer_of_property` below is
        // the separate per-row SPLIT function that shape uses instead.
        discriminator: None,
    },
    EvidenceTerm {
        term: "Psc",
        producers: &["ch_psc"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "Charity",
        producers: &["charity_commission"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "OffshoreEntity",
        producers: &["land_registry_ocod"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "Disqualification",
        producers: &["ch_disqualified"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "DisqualifiedFrom",
        producers: &["ch_disqualified"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "Controls",
        producers: &["ch_psc"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "OffshoreLink",
        // No adapter file constructs `EdgeWeight::OffshoreLink` anywhere (verified by grep across
        // all 15 adapter files) — a legitimate zero-producer entry, not a placeholder.
        producers: &[],
        discriminator: None,
    },
    EvidenceTerm {
        term: "DonatesTo",
        producers: &["electoral_commission"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "TrusteeOf",
        producers: &["charity_commission"],
        discriminator: None,
    },
    EvidenceTerm {
        term: "InLiquidation",
        // Single real producer. The literal matched at the emit site is `EntityStatus::
        // InLiquidation` (set on `NodeWeight::Entity` via `GraphMutation::UpdateNode`), not a
        // `NodeWeight`/`EdgeWeight` variant of its own — there is no such variant.
        producers: &["london_gazette"],
        discriminator: None,
    },
];

/// D2's discriminator for the `PoliticalActor` term: `true` when the JSON payload's
/// `council_name` field is present, which only `council_registers`-sourced rows carry
/// (`electoral_commission` always sets `council_name: None`, `party: Some(...)`; `council_registers`
/// is the reverse — see `crates/schema/src/types/node.rs:154-163`).
pub fn is_council_registers_political_actor(data: &Value) -> bool {
    data.get("council_name").and_then(|v| v.as_str()).is_some()
}

/// D3's per-producer SPLIT label for a `Property` row: `"land_registry_ocod"` when the payload's
/// `country_incorporated` field is present, else `"land_registry_ccod"`.
///
/// `land_registry_ccod` always sets `country_incorporated: None`; `land_registry_ocod` always sets
/// `country_incorporated: Some(rec.country_incorporated)` (`crates/schema/src/types/node.rs:187+`,
/// verified against both adapters' emit sites in this plan's `<interfaces>` section).
///
/// This is distinct in purpose from `discriminator`: `discriminator` GATES whether a row counts
/// as a specific producer's evidence at all; this function never gates anything — it only labels
/// rows that have already been counted, for Plan 33-07's `property_nodes.rs` reporting.
pub fn producer_of_property(data: &Value) -> &'static str {
    if data
        .get("country_incorporated")
        .and_then(|v| v.as_str())
        .is_some()
    {
        "land_registry_ocod"
    } else {
        "land_registry_ccod"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Source-dataset attribution for the entity profile (PIV-03, plan 45-04)
// ─────────────────────────────────────────────────────────────────────────────

/// What can be said about the dataset behind one fact, **from what the store actually records**.
///
/// # Why this is not simply "look up the source column"
///
/// There is no source column. `crates/core/src/pipeline/mod.rs`'s `to_mutation` takes an
/// `IngestEvent` — which carries `source: EventSource` — and produces a `GraphMutation`, which
/// does not. The adapter's identity is validated, normalised and then **dropped** at stage 3, so
/// by the time a row reaches `graph_events` (`event_id`, `kind`, `payload`, `timestamp`) nothing
/// on it names the dataset it came from. Measured 2026-09-15 against `an_anglesey_aebaa785`:
/// 986,062 rows, none carrying a source field of any kind.
///
/// Everything below is therefore an *inference*, and the point of this type is to keep the
/// inference honest. A namespace is attributed only when exactly one producer in this workspace
/// mints it; where two producers mint the same namespace in the same shape, the answer is
/// [`SourceAttribution::Ambiguous`] and the fact is rendered as unattributed rather than given a
/// plausible-looking source. A placeholder provenance is worse than none: it makes an
/// unattributed fact look sourced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAttribution {
    /// Exactly one producer writes facts under this key, verified at the emit site.
    Dataset(&'static str),
    /// More than one producer can have written it, and the store carries nothing to tell them
    /// apart. The string is the reason, and it reaches the analyst verbatim as the
    /// `Unattributed` variant's `reason`.
    Ambiguous(&'static str),
}

impl SourceAttribution {
    /// The dataset name, or `None` when the fact cannot be attributed.
    #[must_use]
    pub fn dataset(self) -> Option<&'static str> {
        match self {
            Self::Dataset(d) => Some(d),
            Self::Ambiguous(_) => None,
        }
    }

    /// The reason attribution failed, or `None` when it did not.
    #[must_use]
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Dataset(_) => None,
            Self::Ambiguous(r) => Some(r),
        }
    }
}

/// One `EntityId` namespace and what, if anything, its prefix says about the producing dataset.
#[derive(Debug, Clone, Copy)]
pub struct IdNamespace {
    /// The literal id prefix, colon included.
    pub prefix: &'static str,
    pub attribution: SourceAttribution,
}

/// Every `EntityId` namespace this workspace mints, and whether the prefix identifies a dataset.
///
/// Emit sites verified live 2026-09-15 by grepping `EntityId(format!(` across
/// `crates/ingestor/src` and `crates/enricher/src`. Namespaces minted by more than one adapter
/// are listed with the reason rather than omitted — an omitted namespace and an ambiguous one
/// would otherwise be indistinguishable to a reader, and only one of them is a decision.
///
/// **`GB-COH:` is ambiguous and that is the single most consequential entry here.** It is minted
/// by `crates/ingestor/src/ch/mapper.rs` (Companies House company profile), by
/// `crates/ingestor/src/cf/mapper.rs` (`party_entity_id`, for any Contracts Finder party
/// carrying a `GB-COH` identifier) and by `crates/enricher/src/adapters/ch_psc.rs`. All three
/// write `NodeWeight::Entity` under the same key, so a company's name and status on this graph
/// may have come from the Companies House register or from a procurement notice's party block,
/// and the store cannot say which. The namespace names a *Companies House identifier*; it does
/// not name the dataset that made the claim.
pub static ID_NAMESPACES: &[IdNamespace] = &[
    IdNamespace {
        prefix: "GB-COH:",
        attribution: SourceAttribution::Ambiguous(
            "the GB-COH: namespace is minted by the Companies House mapper, the Contracts Finder \
             party mapper and the PSC enricher alike, and graph_events records no source",
        ),
    },
    IdNamespace {
        prefix: "CF-PARTY:",
        // crates/ingestor/src/cf/mapper.rs `party_entity_id` is the only mint site.
        attribution: SourceAttribution::Dataset(CONTRACTS_FINDER),
    },
    IdNamespace {
        prefix: "CF:",
        // crates/ingestor/src/cf/mapper.rs, `EntityId(format!("CF:{}", release.ocid))`.
        attribution: SourceAttribution::Dataset(CONTRACTS_FINDER),
    },
    IdNamespace {
        prefix: "planning:",
        // crates/ingestor/src/planning/mapper.rs and crates/ingestor/src/backfill/planning.rs,
        // both of which are the planning-portal path.
        attribution: SourceAttribution::Dataset(PLANNING_AUTHORITY),
    },
    IdNamespace {
        prefix: "GB-MUT:",
        // crates/enricher/src/adapters/mutuals_register.rs is the only mint site.
        attribution: SourceAttribution::Dataset(FCA_MUTUALS),
    },
    IdNamespace {
        prefix: "GB-OCOD:",
        attribution: SourceAttribution::Dataset(HMLR_OCOD),
    },
    IdNamespace {
        prefix: "GB-ROE:",
        attribution: SourceAttribution::Dataset(HMLR_OCOD),
    },
    IdNamespace {
        prefix: "ward:",
        // crates/ingestor/src/planning/mapper.rs mints ward nodes from the planning feed's
        // GSS ward code.
        attribution: SourceAttribution::Dataset(PLANNING_AUTHORITY),
    },
    IdNamespace {
        prefix: "director:",
        attribution: SourceAttribution::Ambiguous(
            "the director: namespace is minted by both the Companies House officer mapper and \
             the planning applicant mapper, through the same slug function, and graph_events \
             records no source",
        ),
    },
    IdNamespace {
        prefix: "location:",
        attribution: SourceAttribution::Ambiguous(
            "location: ids are address-derived and minted by the Companies House, Contracts \
             Finder, planning and FCA Mutuals mappers alike, so one node can be reached from \
             several datasets",
        ),
    },
    IdNamespace {
        prefix: "LOC:",
        attribution: SourceAttribution::Ambiguous(
            "LOC: ids are address-derived and shared by every mapper that normalises an address",
        ),
    },
    IdNamespace {
        prefix: "property:",
        attribution: SourceAttribution::Ambiguous(
            "property: ids are shared by the CCOD, OCOD and PPD extracts; the owning relation's \
             IN side is what distinguishes them",
        ),
    },
    IdNamespace {
        prefix: "political_actor:",
        attribution: SourceAttribution::Ambiguous(
            "political_actor nodes are written by both the Electoral Commission and the council \
             registers adapters — see REGISTRY's PoliticalActor discriminator",
        ),
    },
    IdNamespace {
        prefix: "PA:",
        attribution: SourceAttribution::Ambiguous(
            "political_actor nodes are written by both the Electoral Commission and the council \
             registers adapters — see REGISTRY's PoliticalActor discriminator",
        ),
    },
    IdNamespace {
        prefix: "charity:",
        attribution: SourceAttribution::Dataset(CHARITY_COMMISSION),
    },
];

/// Companies House.
pub const COMPANIES_HOUSE: &str = "CH";
/// Contracts Finder.
pub const CONTRACTS_FINDER: &str = "CF";
/// The Local Planning Authority's planning portal (Arcus on the Anglesey corpus). `LPA` is the
/// glossary token; the platform varies per authority, so the authority is what is named.
pub const PLANNING_AUTHORITY: &str = "LPA";
/// The FCA Mutuals Public Register.
pub const FCA_MUTUALS: &str = "FCA";
/// The Charity Commission register.
pub const CHARITY_COMMISSION: &str = "CC";
/// The Electoral Commission donations register.
pub const ELECTORAL_COMMISSION: &str = "EC";
/// HM Land Registry — Commercial and Corporate Ownership Data.
pub const HMLR_CCOD: &str = "CCOD";
/// HM Land Registry — Overseas Companies Ownership Data.
pub const HMLR_OCOD: &str = "OCOD";
/// ICIJ Offshore Leaks.
pub const ICIJ: &str = "ICIJ";
/// OpenSanctions (via the `yente` matcher).
pub const OPENSANCTIONS: &str = "OpenSanctions";
/// OpenCorporates.
pub const OPENCORPORATES: &str = "OC";
/// This system's own motif detector. Deliberately lowercase and deliberately **not** a dataset
/// abbreviation: a motif alert is something this project asserted, not something a register
/// published, and the chip should not read as if a third party said it.
pub const DETECTOR: &str = "detector";
/// This system's own probe battery. Lowercase for the same reason [`DETECTOR`] is — a probe
/// finding is something this project asserted, not something a register published.
///
/// **Distinct from [`DETECTOR`], and not a synonym for it.** Both name assertions this project
/// made, but they are different producers making different claims: `detector` writes
/// `MotifDetected` events off the graph, while `probe` reports what the audit battery concluded
/// about a whole analysis. A findings-by-entity row (plan 45-05) shows both side by side, so a
/// chip reading `detector` beside a finding from `address-cluster-exact` would name the wrong
/// producer to the analyst reading it.
pub const PROBE: &str = "probe";

/// The dataset an id's namespace identifies, or the reason it identifies none.
///
/// Longest-prefix match, so `GB-OCOD:` is not shadowed by a shorter entry. An id whose namespace
/// is not in [`ID_NAMESPACES`] at all is [`SourceAttribution::Ambiguous`] — never a guess, and
/// never silently `CH`.
#[must_use]
pub fn dataset_of_id(id: &str) -> SourceAttribution {
    ID_NAMESPACES
        .iter()
        .filter(|ns| id.starts_with(ns.prefix))
        .max_by_key(|ns| ns.prefix.len())
        .map_or(
            SourceAttribution::Ambiguous(
                "the id carries no namespace this workspace is known to mint, so no producer can \
                 be named",
            ),
            |ns| ns.attribution,
        )
}

/// Edge/node terms that cannot be attributed **whatever** namespace they are read through,
/// because two producers write them in the same shape.
///
/// `Directs` is the whole list today, and it is the same refusal this module's own doc comment
/// already records for [`REGISTRY`]: `crates/enricher/src/adapters/planning_identity.rs` and
/// `crates/ingestor/src/ch/mapper.rs` both emit `Directs` edges, both with a `director:` source
/// and a `GB-COH:` destination. Measured 2026-09-15 on `an_anglesey_aebaa785`: of 24,662 Directs
/// events, 24,503 run `GB-COH:` → `director:` and 159 run `director:` → `GB-COH:`, and no field
/// on either form names its writer.
pub static UNATTRIBUTABLE_TERMS: &[(&str, &str)] = &[(
    "Directs",
    "Directs edges are written by both the Companies House officer mapper and the \
     planning-identity join, in the same shape, and graph_events records no source",
)];

/// The reason `term` can never be attributed, or `None` if it can be.
#[must_use]
pub fn term_ambiguity(term: &str) -> Option<&'static str> {
    UNATTRIBUTABLE_TERMS
        .iter()
        .find(|(t, _)| *t == term)
        .map(|(_, reason)| *reason)
}

/// The dataset behind a projected relation row.
///
/// `in_id` is the relation's IN side as a record string; it is what distinguishes the two HM Land
/// Registry extracts on `owns_property` (`offshore_entity:` ⇒ OCOD, `entity:` ⇒ CCOD). Measured
/// 2026-09-15: 98,433 of 108,026 `owns_property` rows are `offshore_entity:`-sided and 9,593 are
/// `entity:`-sided, with nothing in between.
#[must_use]
pub fn dataset_of_relation(table: &str, in_id: &str) -> SourceAttribution {
    match table {
        "owns_property" => {
            if in_id.starts_with("offshore_entity:") {
                SourceAttribution::Dataset(HMLR_OCOD)
            } else if in_id.starts_with("entity:") {
                SourceAttribution::Dataset(HMLR_CCOD)
            } else {
                SourceAttribution::Ambiguous(
                    "the owns_property row's IN side is neither an entity nor an offshore_entity, \
                     so neither Land Registry extract can be named",
                )
            }
        }
        // REGISTRY: TrusteeOf has exactly one producer, charity_commission.
        "trustee_of" => SourceAttribution::Dataset(CHARITY_COMMISSION),
        // REGISTRY: DonatesTo has exactly one producer, electoral_commission.
        "donates_to" => SourceAttribution::Dataset(ELECTORAL_COMMISSION),
        // REGISTRY: Controls has exactly one producer, ch_psc.
        "controls" => SourceAttribution::Dataset(COMPANIES_HOUSE),
        // REGISTRY records OffshoreLink as a legitimate ZERO-producer entry: no adapter file
        // constructs EdgeWeight::OffshoreLink anywhere. A table with no producer cannot have one
        // named, and the live table holds 0 rows (measured 2026-09-15).
        "offshore_link" => SourceAttribution::Ambiguous(
            "no adapter in this workspace produces an offshore_link edge, so no dataset can be \
             named for one",
        ),
        _ => SourceAttribution::Ambiguous(
            "this relation table is not in the attribution registry, so its producer is unknown",
        ),
    }
}

/// The glossary token for an `identity_links` adapter name, or `None` when this registry has no
/// mapping for it.
///
/// `identity_links` is the one family of facts on the profile whose provenance is **recorded
/// rather than inferred**: the table carries `source`, `resolved_at` and `status` columns, so the
/// producing adapter, the time and the confidence tier are all read straight off the row. This
/// function only translates the adapter's name into the short token the provenance chip renders.
///
/// `None` is **not** "unattributed". The caller holds the raw `source` string, and an adapter
/// naming itself is a real attribution — see
/// `crates/core::persistence::queries::identity_link_provenance`, which falls back to the raw
/// name rather than to a placeholder. `planning_identity` is the live example: 182 rows on
/// `an_anglesey_aebaa785`, measured 2026-09-15, with no glossary token of their own.
#[must_use]
pub fn dataset_of_identity_link_source(source: &str) -> Option<&'static str> {
    match source {
        "ch_psc" | "ch_disqualified" => Some(COMPANIES_HOUSE),
        "charity_commission" => Some(CHARITY_COMMISSION),
        "electoral_commission" => Some(ELECTORAL_COMMISSION),
        "land_registry" | "land_registry_ccod" => Some(HMLR_CCOD),
        "land_registry_ocod" => Some(HMLR_OCOD),
        "opensanctions" => Some(OPENSANCTIONS),
        "icij" => Some(ICIJ),
        "opencorporates" => Some(OPENCORPORATES),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_exactly_thirteen_entries_and_no_directs() {
        // See the deviation note on REGISTRY: the plan's own <action> enumerates 13 verified
        // terms even though its frontmatter/acceptance-criteria say 12 — the enumerated,
        // evidence-backed list is the operative spec.
        assert_eq!(REGISTRY.len(), 13, "REGISTRY must hold exactly 13 entries");
        assert!(
            REGISTRY.iter().all(|e| e.term != "Directs"),
            "Directs must never appear in REGISTRY — see the module doc comment for why"
        );
    }

    #[test]
    fn offshore_link_is_a_legitimate_zero_producer_entry() {
        let entry = REGISTRY
            .iter()
            .find(|e| e.term == "OffshoreLink")
            .expect("OffshoreLink entry must exist");
        assert!(entry.producers.is_empty());
    }

    #[test]
    fn in_liquidation_has_single_producer_london_gazette() {
        let entry = REGISTRY
            .iter()
            .find(|e| e.term == "InLiquidation")
            .expect("InLiquidation entry must exist");
        assert_eq!(entry.producers, &["london_gazette"]);
    }

    #[test]
    fn council_sourced_political_actor_discriminates_true() {
        let data = serde_json::json!({
            "name": "Cllr Jane Smith",
            "party": null,
            "council_name": "Anglesey Council",
        });
        assert!(is_council_registers_political_actor(&data));
    }

    #[test]
    fn ec_sourced_political_actor_discriminates_false() {
        let data = serde_json::json!({
            "name": "Jane Smith",
            "party": "Labour Party",
            "council_name": null,
        });
        assert!(!is_council_registers_political_actor(&data));
    }

    #[test]
    fn ccod_property_labels_as_land_registry_ccod() {
        let data = serde_json::json!({
            "title_number": "AB12345",
            "country_incorporated": null,
        });
        assert_eq!(producer_of_property(&data), "land_registry_ccod");
    }

    #[test]
    fn ocod_property_labels_as_land_registry_ocod() {
        let data = serde_json::json!({
            "title_number": "AB12345",
            "country_incorporated": "Jersey",
        });
        assert_eq!(producer_of_property(&data), "land_registry_ocod");
    }

    // ── Source-dataset attribution (PIV-03, plan 45-04) ─────────────────────

    /// The consequential entry: `GB-COH:` cannot name a dataset, because three producers mint it.
    ///
    /// PROVE THIS CAN FAIL: give the `GB-COH:` entry
    /// `SourceAttribution::Dataset(COMPANIES_HOUSE)` and this assertion fails — which is exactly
    /// the placeholder attribution plan 45-04 forbids, since a Contracts Finder party block also
    /// writes `NodeWeight::Entity` under this key.
    #[test]
    fn gb_coh_is_ambiguous_because_three_producers_mint_it() {
        let got = dataset_of_id("GB-COH:99000001");
        assert_eq!(got.dataset(), None, "GB-COH: must not name a dataset");
        let reason = got
            .reason()
            .expect("an ambiguous namespace must carry a reason");
        assert!(
            reason.contains("Contracts Finder"),
            "the reason must name the other producer, got: {reason}"
        );
    }

    /// Longest-prefix match: `GB-OCOD:` must not be shadowed by a shorter entry, and the four
    /// single-producer namespaces must each resolve to their own dataset.
    #[test]
    fn single_producer_namespaces_resolve_to_their_own_dataset() {
        for (id, expected) in [
            ("CF:ocds-b5fd17-0001", CONTRACTS_FINDER),
            ("CF-PARTY:GB-CFS-123", CONTRACTS_FINDER),
            ("planning:28C302A/LB", PLANNING_AUTHORITY),
            ("GB-MUT:12345R", FCA_MUTUALS),
            ("GB-OCOD:panama-pepino-business-sa", HMLR_OCOD),
            ("charity:1234567", CHARITY_COMMISSION),
        ] {
            assert_eq!(
                dataset_of_id(id).dataset(),
                Some(expected),
                "{id} must attribute to {expected}"
            );
        }
    }

    /// The two namespaces that carry the bulk of this corpus's profile rows are both ambiguous,
    /// and each says why. This is the measurement plan 45-04 reports, pinned as a claim.
    #[test]
    fn director_and_location_namespaces_are_ambiguous_with_reasons() {
        for id in ["director:jane-smith", "location:1-high-st,-llangefni"] {
            let got = dataset_of_id(id);
            assert_eq!(got.dataset(), None, "{id} must not name a dataset");
            assert!(
                got.reason().is_some_and(|r| r.len() > 20),
                "{id} must carry a substantive reason"
            );
        }
    }

    /// An id in no known namespace is ambiguous, never a default.
    #[test]
    fn an_unknown_namespace_is_ambiguous_not_a_default() {
        let got = dataset_of_id("wat:1");
        assert_eq!(got.dataset(), None);
        assert!(got.reason().is_some());
    }

    /// `Directs` can never be attributed, whatever namespace it is read through — the same
    /// refusal this module's doc comment already records for REGISTRY.
    #[test]
    fn directs_is_unattributable_and_nothing_else_is() {
        assert!(term_ambiguity("Directs").is_some());
        for term in ["AwardedTo", "LocatedAt", "Contract", "Entity", "Location"] {
            assert!(
                term_ambiguity(term).is_none(),
                "{term} must not be in UNATTRIBUTABLE_TERMS without evidence"
            );
        }
    }

    /// The two Land Registry extracts are distinguished by the owning relation's IN side, which
    /// is the only thing on an `owns_property` row that can tell them apart.
    #[test]
    fn owns_property_splits_ccod_from_ocod_on_the_in_side() {
        assert_eq!(
            dataset_of_relation("owns_property", "offshore_entity:`GB-OCOD:x`").dataset(),
            Some(HMLR_OCOD)
        );
        assert_eq!(
            dataset_of_relation("owns_property", "entity:`GB-COH:02644862`").dataset(),
            Some(HMLR_CCOD)
        );
        assert_eq!(
            dataset_of_relation("owns_property", "ward:W05001498").dataset(),
            None,
            "an IN side that is neither cannot name an extract"
        );
    }

    /// `offshore_link` has no producer at all (REGISTRY's zero-producer entry), so it names no
    /// dataset — the difference between "we don't know" and "there is nothing to know" is in the
    /// reason, not in a fabricated source.
    #[test]
    fn offshore_link_names_no_dataset_because_it_has_no_producer() {
        let got = dataset_of_relation("offshore_link", "entity:`GB-COH:1`");
        assert_eq!(got.dataset(), None);
        assert!(got.reason().is_some_and(|r| r.contains("no adapter")));
    }

    /// Single-producer relation tables attribute to their one producer, taken from REGISTRY.
    #[test]
    fn single_producer_relations_attribute_to_their_registry_producer() {
        assert_eq!(
            dataset_of_relation("trustee_of", "entity:`GB-COH:1`").dataset(),
            Some(CHARITY_COMMISSION)
        );
        assert_eq!(
            dataset_of_relation("donates_to", "entity:`GB-COH:1`").dataset(),
            Some(ELECTORAL_COMMISSION)
        );
        assert_eq!(
            dataset_of_relation("controls", "psc:`x`").dataset(),
            Some(COMPANIES_HOUSE)
        );
    }

    /// Every live `identity_links.source` value on `an_anglesey_aebaa785` (measured 2026-09-15)
    /// either maps to a glossary token or is deliberately unmapped so the caller can fall back to
    /// the adapter's own name.
    #[test]
    fn live_identity_link_sources_map_or_are_deliberately_unmapped() {
        for (source, expected) in [
            ("ch_psc", Some(COMPANIES_HOUSE)),
            ("charity_commission", Some(CHARITY_COMMISSION)),
            ("electoral_commission", Some(ELECTORAL_COMMISSION)),
            ("land_registry", Some(HMLR_CCOD)),
            ("land_registry_ocod", Some(HMLR_OCOD)),
            ("planning_identity", None),
        ] {
            assert_eq!(
                dataset_of_identity_link_source(source),
                expected,
                "{source} mapping changed"
            );
        }
    }

    /// The detector token is deliberately not a dataset abbreviation.
    #[test]
    fn the_detector_token_does_not_read_as_a_third_party_register() {
        assert_eq!(DETECTOR, "detector");
        assert!(
            DETECTOR.chars().all(|c| !c.is_ascii_uppercase()),
            "an all-caps token would render as a source register in the provenance chip"
        );
    }

    /// The probe token follows the same rule as `DETECTOR`, and is not the same token.
    ///
    /// PROVE THIS CAN FAIL: set `PROBE` to `DETECTOR`'s value and the inequality fails — which is
    /// the drift this guards, because the two producers render in the same chip column and a
    /// reader cannot tell them apart if they share a string.
    #[test]
    fn the_probe_token_is_lowercase_and_distinct_from_the_detector_token() {
        assert_eq!(PROBE, "probe");
        assert!(
            PROBE.chars().all(|c| !c.is_ascii_uppercase()),
            "an all-caps token would render as a source register in the provenance chip"
        );
        assert_ne!(
            PROBE, DETECTOR,
            "a probe finding and a motif alert are different producers; one token for both names \
             the wrong one on whichever row it is wrong for"
        );
    }
}
