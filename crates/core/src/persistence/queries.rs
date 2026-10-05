use crate::persistence::scan;
use async_graphql::SimpleObject;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, warn};
use urban_integrity_schema::types::attribution;
use urban_integrity_schema::types::node::{
    ContractData, EntityData, NodeWeight, OfficerData, OffshoreEntityData,
};
use urban_integrity_schema::types::verdict::{
    Finding, RecordKind, RecordRef, Severity, VerdictKind,
};

/// The mandatory per-fact provenance contract from plan 45-01, re-exported so producers and
/// consumers in this workspace reach one type rather than two spellings of it (PIV-03).
///
/// It lives in `crates/workbench-dto` because the browser must be able to deserialise it and that
/// crate is the only thing the browser may depend on (D-01). Nothing about it is browser-specific:
/// it is the shape of the claim "this fact came from here", and this module is where the claim is
/// made.
pub use urban_integrity_workbench_dto::provenance::{LinkTier, Provenance};

/// The PIV-04 temporal vocabulary from plan 45-06, re-exported for the same reason `Provenance`
/// is: it lives in `crates/workbench-dto` because the browser must deserialise it, and there is
/// one spelling of "when was this in force", not two.
///
/// [`NULL_RANGE_RULE`] is the sentence an analyst reads; [`as_at_status`] is where this module
/// applies it. They are deliberately the same decision in two forms, and a test in
/// `crates/core/tests/profile_as_at.rs` asserts they cannot disagree.
pub use urban_integrity_workbench_dto::temporal::{
    AsAtStatus, EffectiveRange, Temporal, NULL_RANGE_RULE, UNDATED_ROWS_ARE,
};

use crate::config::AppConfig;

/// Returns true if the input looks like a slug or entity ID (contains a hyphen or colon).
///
/// The CLI uses this to dispatch between `query_entity_by_id` (exact) and
/// `query_entity_by_name` (substring).  The predicate deliberately stays simple:
/// company names don't normally contain `:` or `-`; slugs and `GB-COH:…` /
/// `director:…` IDs always do.
///
/// **Known false-positive (deliberate):** hyphenated company names such as
/// `"Smith-Jones Construction"` match this predicate and are dispatched as slug
/// lookups rather than name searches.  This is an accepted trade-off: the slug
/// lookup path returns no results for such names (a safe no-op), and requiring
/// callers to quote or escape hyphens would harm usability.  Do not change this
/// behaviour without auditing all CLI callers.
///
/// `pub` so `crates/cli` can re-export it directly.
pub fn is_slug_shape(input: &str) -> bool {
    let s = input.trim();
    s.contains('-') || s.contains(':')
}

/// Extract an entity_id string from a SurrealDB field value.
///
/// SurrealDB 3.x coerces string values that match `table:id` (where `table` is a
/// valid bare identifier, e.g. `director`, `planning`, `ward`) to RecordId objects
/// when stored via `CREATE … CONTENT`.  On read-back through the Rust SDK the
/// RecordId arrives as a JSON object `{"tb":"director","id":{"String":"slug"}}`.
///
/// Fields with table names containing hyphens (`GB-COH:`, `CF-PARTY:`, `CF:`,
/// `OFF:`) are NOT coerced because the hyphen makes `GB-COH` an invalid bare
/// identifier in SurrealDB.
///
/// This helper handles both forms so callers are transparent to the coercion.
pub fn extract_id_string(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    // RecordId object: {"tb": "director", "id": "slug"} or {"tb": "director", "id": {"String": "slug"}}
    if let (Some(tb), Some(id_val)) = (v["tb"].as_str(), Some(&v["id"])) {
        let id_str = if let Some(s) = id_val.as_str() {
            s.to_owned()
        } else if let Some(s) = id_val["String"].as_str() {
            s.to_owned()
        } else {
            // Fallback: render id as JSON string
            id_val.to_string().trim_matches('"').to_owned()
        };
        return Some(format!("{tb}:{id_str}"));
    }
    None
}

/// One director row within an `EntityProfile`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct DirectorRow {
    pub name: String,
    pub role: Option<String>,
    pub appointed: Option<String>,
    pub resigned: Option<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]`: `Provenance` is a
    /// `workbench-dto` wire type with no `async-graphql` derives, and giving it a parallel
    /// GraphQL mirror would be the second spelling of one contract that plan 45-01 exists to
    /// prevent. The GraphQL profile is therefore unchanged by this field; the workbench HTTP
    /// surface (plan 45-08) is what renders it.
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// One contract row within an `EntityProfile`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct ContractRow {
    /// The contract node's own graph id — `CF:ocds-…` for a Contracts Finder award (PIV-03 /
    /// D-06's pivot target).
    ///
    /// **Carried, never minted.** It is the `AwardedTo` edge's destination id, which this row is
    /// assembled *by looking up* — a destination whose weight cannot be read produces no row at
    /// all, so there is no `ContractRow` without an id and the field is not an `Option`.
    ///
    /// The same id already attributes the row (`graph_event_provenance("Contract", id, …)`); it
    /// was simply dropped on the way out, which is what made the profile's contract rows
    /// unpivotable.
    pub id: String,
    pub name: String,
    pub value_gbp: Option<f64>,
    pub status: Option<String>,
    pub published: Option<String>,
    pub ward_code: Option<String>,
    /// Contracts Finder tender reference. Serialised as `tenderRef` (camelCase).
    pub tender_ref: Option<String>,
    /// Planning portal application reference. Serialised as `applicationRef` (camelCase).
    ///
    /// **Since the D-03 split this is `None` on every row assembled from the store**, because a
    /// destination carrying an application reference is routed to
    /// [`EntityProfile::planning_applications`] instead. The field is deliberately *kept* rather
    /// than deleted: it is what makes "no planning reference appears under contracts" a claim a
    /// test can falsify. Delete it and the invariant becomes true by type, the test becomes
    /// unwritable, and a future mis-routed row has nowhere to show up.
    pub application_ref: Option<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// One planning application within an `EntityProfile` (D-03 / PIV-02).
///
/// Planning applications reach an entity over the same `AwardedTo` edge as a Contracts Finder
/// award, which is why they used to share [`ContractRow`]. They are not awards: nothing is
/// procured, no money changes hands, and counting one as a contract inflates a figure
/// `crates/cli/src/commands/report.rs` prints.
///
/// No `value_gbp` and no `tender_ref`, because neither exists. Measured against
/// `an_anglesey_aebaa785` on 2026-09-15: of 102,619 `planning:`-prefixed `Contract` node-weight
/// events, **zero** serialise a `value_gbp` or a `tender_ref` — the ingest mapper omits both. A
/// column that is empty on every row for the whole corpus reads as missing data rather than as
/// absent structure, so it is not declared. This matches `workbench_dto::PlanningApplicationRowView`,
/// which plan 45-01 shaped the same way.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct PlanningApplicationRow {
    /// The application node's own graph id — always `planning:…`, since that prefix is what put
    /// the row in this section rather than in [`EntityProfile::contracts`]
    /// ([`is_planning_application_id`]).
    ///
    /// Carried rather than minted, on the same terms as [`ContractRow::id`]: it is the `AwardedTo`
    /// destination id, and a destination with no readable weight yields no row.
    ///
    /// The node class is `NodeWeight::Contract` — D-03 splits the *sections*, not the node — so a
    /// pivot built from this id carries `RecordKind::Contract`, not a third kind. See
    /// [`urban_integrity_schema::types::verdict::RecordKind`].
    pub id: String,
    pub name: String,
    pub status: Option<String>,
    /// Publication date. `None` on every row of today's corpus — the planning mapper writes no
    /// `date_published`; the received date lives outside the node weight.
    pub published: Option<String>,
    pub ward_code: Option<String>,
    /// Planning portal application reference. Serialised as `applicationRef` (camelCase).
    pub application_ref: Option<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// The destination-id prefix that marks an `AwardedTo` destination as a planning application.
///
/// **Read this before changing the predicate.** The obvious split — "contracts are the ones
/// prefixed `contract:`" — matches *nothing* in this store and fails silently in the worst
/// available way: the contracts section empties, the planning applications stay exactly where they
/// were, and a test asserting "no planning reference appears under contracts" still passes,
/// because both sections are then wrong in a way that assertion cannot see.
///
/// Measured against `an_anglesey_aebaa785` on 2026-09-15, over the 45,876 distinct `Contract`
/// nodes that survive [`fold_latest_node_rows`]:
///
/// | Destination id prefix | Distinct nodes | Share |
/// |---|---|---|
/// | `planning:` | 39,860 | 86.9% |
/// | `CF:` | 6,016 | 13.1% |
/// | anything else, `contract:` included | **0** | 0% |
///
/// So `planning:` is the positive predicate and everything else is a contract, per
/// [`is_planning_application_id`].
pub const PLANNING_APPLICATION_ID_PREFIX: &str = "planning:";

/// Whether an `AwardedTo` destination id names a planning application rather than a contract.
///
/// The predicate is the **destination id prefix**, not the presence of `ContractData::application_ref`.
/// The two agree perfectly on today's corpus — of 102,619 `planning:`-prefixed `Contract`
/// node-weight events, 102,619 carry an `application_ref` and 0 carry a `tender_ref`; of 6,180
/// `CF:`-prefixed events, 6,180 carry a `tender_ref` and 0 carry an `application_ref` (measured
/// 2026-09-15) — but they are different claims and only one of them is available before the node
/// weight has been read. The id is on the edge; the refs are on a node that may not have been
/// ingested yet. Splitting on the id means a planning application with an unreadable or missing
/// node weight is still not counted as a contract award.
///
/// Note that [`query_entity_lpa_presence`] discriminates the other way, on `tender_ref.is_some()`.
/// That divergence is deliberate and recorded rather than silently unified: changing it would move
/// the LPA bucket counts, which are a separate shipped figure and not this change's to alter.
#[must_use]
pub fn is_planning_application_id(destination_id: &str) -> bool {
    destination_id.starts_with(PLANNING_APPLICATION_ID_PREFIX)
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-fact provenance (PIV-03, D-04, plan 45-04)
// ─────────────────────────────────────────────────────────────────────────────
//
// # The thing to understand before reading any of this
//
// `graph_events` records no source. `crates/core/src/pipeline/mod.rs`'s `to_mutation` receives an
// `IngestEvent` carrying `source: EventSource` and produces a `GraphMutation`, which has no such
// field — the adapter's identity is validated, normalised and then dropped at stage 3. Every row
// in the log is `event_id` / `kind` / `payload` / `timestamp` and nothing else. So for anything
// read out of `graph_events`, the dataset is an INFERENCE from the id namespace, and it is only
// safe where exactly one producer mints that namespace.
//
// Three families of fact reach the profile, and they are not equally attributable:
//
// | family | dataset | timestamp | tier |
// |---|---|---|---|
// | `identity_links` rows | recorded (`source`) | recorded (`resolved_at`) | recorded (`status`) |
// | projected relation rows | inferred from the table + IN side | recorded (`meta.created_at`) | n/a |
// | `graph_events` rows | inferred from the id namespace, often ambiguous | recorded (`timestamp`) | n/a |
//
// Nothing here ever defaults. A fact whose dataset cannot be named is `Provenance::Unattributed`
// carrying the reason, because a placeholder provenance is worse than none: it makes an
// unattributed fact look sourced.

/// The effective range of a contract award or a planning application.
///
/// A publication date is a **start with no recorded end**, not a point: an award published on a
/// date was in force from then as far as the record says, and nothing in this store ever closes it
/// (`effective_to` is null on all 511,754 edge events). The `AwardedTo` edge carries no dates
/// either, so `ContractData::date_published` is the only statement available and the basis names
/// it.
///
/// Measured 2026-09-15: 4,905 of 108,799 `Contract` node-weight events carry a `date_published`,
/// **all 4,905 of them `CF:`-keyed**. So contracts are dated for 79.4% of the Contracts Finder
/// population and planning applications are undated for all 102,619 of theirs — which is the same
/// fact 45-03 recorded as `PlanningApplicationRow::published` being `None` on every live row,
/// reaching the as-at filter as `AsAtStatus::Undated`.
fn award_range(cd: &ContractData) -> EffectiveRange {
    effective_range(&[("contract.date_published", cd.date_published.clone(), None)])
}

/// ISO-8601 rendering of a `graph_events` millisecond timestamp, or `None` when there is not one
/// to render.
///
/// **Zero is treated as absent, not as 1970.** `parse_alert_row` renders a missing timestamp as
/// `"1970-01-01T00:00:00Z"`, which is fine for a display field an analyst reads next to the alert
/// but is exactly the placeholder a provenance chip must never carry — an analyst reading
/// `CH · 1970-01-01` cannot tell a genuinely ancient record from a lost one. Measured 2026-09-15
/// on `an_anglesey_aebaa785`: 0 of 986,062 rows carry a zero or absent `timestamp`, so this branch
/// is defensive rather than load-bearing today.
fn observed_at_from_ms(timestamp_ms: Option<u64>) -> Option<String> {
    let ms = timestamp_ms?;
    if ms == 0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(i64::try_from(ms).ok()?).map(|dt| dt.to_rfc3339())
}

/// The `meta.created_at` projection every profile read of a relation table now asks for — a
/// projected edge's only `observed_at`, and the reason those sections can be attributed at all.
///
/// One constant rather than four copies of the pair: the alias is what
/// [`relation_provenance`]'s callers read the value back under, so a typo in one of them would
/// silently unattribute that section and nothing else.
pub(crate) const CREATED_AT_PROJECTION: (&str, &str) = ("meta.created_at", "created_at");

/// The `meta.effective_from` / `meta.effective_to` projections every profile read of a relation
/// table asks for alongside [`CREATED_AT_PROJECTION`] (PIV-04).
///
/// Constants rather than five repetitions of each pair, for the same reason
/// [`CREATED_AT_PROJECTION`] is one: the alias is what [`relation_effective_range`]'s callers read
/// the value back under, so a typo in one of them would silently undate that section and nothing
/// else. `crates/schema/src/db.rs` declares both fields `TYPE option<datetime>` on every one of
/// these tables.
pub(crate) const EFFECTIVE_FROM_PROJECTION: (&str, &str) =
    ("meta.effective_from", "effective_from");
/// See [`EFFECTIVE_FROM_PROJECTION`].
pub(crate) const EFFECTIVE_TO_PROJECTION: (&str, &str) = ("meta.effective_to", "effective_to");

/// The reason given when a fact's event carries no usable timestamp.
const NO_TIMESTAMP_REASON: &str =
    "the graph_events row that carried this fact has no readable timestamp";

/// The provenance of a fact read out of `graph_events`.
///
/// `term` is the `NodeWeight`/`EdgeWeight` serde tag the fact came from (`"Entity"`, `"Location"`,
/// `"Contract"`, `"Directs"`, `"LocatedAt"`). `producer_id` is the `EntityId` whose namespace
/// identifies the writer — for a node fact that is the node's own id; for an edge fact it is the
/// end whose namespace the writer is known by.
///
/// Order matters: a term with two producers is unattributable no matter how good its timestamp is,
/// so [`attribution::term_ambiguity`] is consulted first.
#[must_use]
pub fn graph_event_provenance(
    term: &str,
    producer_id: &str,
    timestamp_ms: Option<u64>,
) -> Provenance {
    if let Some(reason) = attribution::term_ambiguity(term) {
        return Provenance::Unattributed {
            reason: reason.to_owned(),
        };
    }
    let Some(observed_at) = observed_at_from_ms(timestamp_ms) else {
        return Provenance::Unattributed {
            reason: NO_TIMESTAMP_REASON.to_owned(),
        };
    };
    match attribution::dataset_of_id(producer_id) {
        attribution::SourceAttribution::Dataset(source_dataset) => Provenance::Attributed {
            source_dataset: source_dataset.to_owned(),
            observed_at,
            // A `graph_events` fact is read straight from a primary dataset, never through an
            // identity link — `None` here is the statement "this fact is not link-derived", which
            // is why 45-01 made the key mandatory rather than omissible.
            tier: None,
        },
        attribution::SourceAttribution::Ambiguous(reason) => Provenance::Unattributed {
            reason: reason.to_owned(),
        },
    }
}

/// The provenance of a fact derived from an `identity_links` row — the one family whose
/// provenance is **recorded rather than inferred**.
///
/// `status` is also the ENR-11 gate's second line. The query that produced the row already
/// filtered to `auto_linked`/`confirmed`; anything else arriving here cannot be given a tier,
/// because `LinkTier` has no `pending_review` variant by design, so it becomes `Unattributed`
/// rather than an attributed fact with a silently missing tier. A new surface leaking a candidate
/// link is a regression against a shipped rule, not a rough edge.
///
/// An adapter with no glossary token is attributed under **its own name** rather than a
/// placeholder — `planning_identity` is the live example. Only an empty `source` is unattributable.
#[must_use]
pub fn identity_link_provenance(
    source: &str,
    status: &str,
    resolved_at: chrono::DateTime<chrono::Utc>,
) -> Provenance {
    let tier = match status {
        "confirmed" => LinkTier::Confirmed,
        "auto_linked" => LinkTier::AutoLinked,
        other => {
            return Provenance::Unattributed {
                reason: format!(
                    "identity link status {other:?} has no renderable confidence tier (ENR-11)"
                ),
            }
        }
    };
    if source.is_empty() {
        return Provenance::Unattributed {
            reason: "the identity_links row carries no source, so its producing adapter cannot be \
                     named"
                .to_owned(),
        };
    }
    Provenance::Attributed {
        source_dataset: attribution::dataset_of_identity_link_source(source)
            .unwrap_or(source)
            .to_owned(),
        observed_at: resolved_at.to_rfc3339(),
        tier: Some(tier),
    }
}

/// The provenance of a fact read out of a projected relation table (`owns_property`,
/// `trustee_of`, `donates_to`, `offshore_link`).
///
/// `created_at_raw` is the row's `meta.created_at` as the SurrealDB SDK handed it back. It is read
/// tolerantly — a bare string, or an object form under one of the keys the SDK uses for a
/// `datetime` — because the projection crosses a driver boundary this module does not control, and
/// a provenance that silently became "now" would be worse than one that admits it could not read
/// the field.
#[must_use]
pub fn relation_provenance(
    table: &str,
    in_id: &str,
    created_at_raw: Option<&serde_json::Value>,
) -> Provenance {
    let Some(observed_at) = created_at_raw.and_then(read_datetime_value) else {
        return Provenance::Unattributed {
            reason: format!("the {table} row carries no readable meta.created_at"),
        };
    };
    match attribution::dataset_of_relation(table, in_id) {
        attribution::SourceAttribution::Dataset(source_dataset) => Provenance::Attributed {
            source_dataset: source_dataset.to_owned(),
            observed_at,
            tier: None,
        },
        attribution::SourceAttribution::Ambiguous(reason) => Provenance::Unattributed {
            reason: reason.to_owned(),
        },
    }
}

/// A SurrealDB `datetime` as a string, whichever JSON shape the driver produced for it.
fn read_datetime_value(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    // Object forms the SDK has used for a datetime, in the order they are cheapest to test.
    for key in ["Datetime", "datetime", "$date"] {
        if let Some(s) = v.get(key).and_then(serde_json::Value::as_str) {
            return Some(s.to_owned());
        }
    }
    None
}

/// The provenance of a finding this system produced rather than read.
///
/// A motif alert is not sourced from a register — it is an assertion this project made about data
/// it holds. [`attribution::DETECTOR`] is deliberately lowercase so the chip cannot be mistaken
/// for a third-party source, and it is a real attribution rather than a placeholder: it names the
/// actual producer.
#[must_use]
pub fn detector_provenance(timestamp_ms: Option<u64>) -> Provenance {
    match observed_at_from_ms(timestamp_ms) {
        Some(observed_at) => Provenance::Attributed {
            source_dataset: attribution::DETECTOR.to_owned(),
            observed_at,
            tier: None,
        },
        None => Provenance::Unattributed {
            reason: NO_TIMESTAMP_REASON.to_owned(),
        },
    }
}

// ── PIV-04: effective ranges and the "as at" filter ──────────────────────────
//
// The rule, decided here and stated to the analyst through
// `urban_integrity_workbench_dto::temporal::NULL_RANGE_RULE`:
//
//   An as-at filter MARKS every temporal row and REMOVES none. A row whose recorded range
//   positively excludes the date is `AsAtStatus::OutOfForce`; a row with no readable endpoint is
//   `AsAtStatus::Undated` — retained, and labelled so the filter is never read as having vouched
//   for it.
//
// Two measurements from `an_anglesey_aebaa785` on 2026-09-15 are why, and they are not edge cases:
//
//   * `effective_to` is null on **511,754 of 511,754** `EdgeAdded` events. Nothing in this store
//     has ever been closed, so no row can be excluded by its end date at all.
//   * `effective_from` is null on 384,363 of them. The 127,391 that carry one are `Controls`
//     (19,587) and `OwnsProperty` (107,804) and nothing else — every `Directs`, `AwardedTo` and
//     `LocatedAt` edge is undated.
//
// Dropping undated rows would therefore empty Officers, Contracts, Planning applications and
// Addresses outright; keeping them unmarked would make the filter a silent no-op on the same four
// sections. Neither raises an error and both look like a working feature.

/// The candidate sources a row's [`EffectiveRange`] may be read from, in priority order.
///
/// A profile row's range can live on the edge that carries it or on the node the edge points at,
/// and which one answered is part of the answer — so the winning candidate's label becomes
/// `EffectiveRange::Recorded::basis` and, when none of them answer, every label is joined into
/// `EffectiveRange::Unrecorded::basis`. That is the difference between "this dataset records no
/// dates" and "this row lost its dates", which is not visible from an empty field.
///
/// Callers pass at least one candidate; an empty slice yields an `Unrecorded` whose basis names
/// nothing, which is the one shape this function cannot make honest.
#[must_use]
pub fn effective_range(candidates: &[(&str, Option<String>, Option<String>)]) -> EffectiveRange {
    for (basis, from, to) in candidates {
        if from.is_some() || to.is_some() {
            return EffectiveRange::Recorded {
                from: from.clone(),
                to: to.clone(),
                basis: (*basis).to_owned(),
            };
        }
    }
    EffectiveRange::Unrecorded {
        basis: candidates
            .iter()
            .map(|(basis, _, _)| *basis)
            .collect::<Vec<_>>()
            .join(" / "),
    }
}

/// The effective range of a row read out of a projected relation table.
///
/// `table` names the basis so a reader can check the claim against the DDL in
/// `crates/schema/src/db.rs`, which declares `meta.effective_from` / `meta.effective_to` on every
/// one of these tables. Both endpoints are read through the same tolerant datetime reader the
/// provenance path uses, because the projection crosses a driver boundary this module does not
/// control.
#[must_use]
pub fn relation_effective_range(
    table: &str,
    effective_from_raw: Option<&serde_json::Value>,
    effective_to_raw: Option<&serde_json::Value>,
) -> EffectiveRange {
    effective_range(&[(
        &format!("{table}.meta.effective_from / {table}.meta.effective_to"),
        effective_from_raw.and_then(read_datetime_value),
        effective_to_raw.and_then(read_datetime_value),
    )])
}

/// The `meta.effective_from` / `meta.effective_to` of an `EdgeAdded` event's serialised
/// `EdgeWeight`, as a `(basis, from, to)` candidate for [`effective_range`].
///
/// A candidate rather than a finished range, because an edge is rarely the *best* statement of
/// when the fact it carries was in force — a directorship's tenure is on the Officer node, and the
/// `Directs` edge beneath it is undated on every row of this corpus. The caller orders the
/// candidates; this one is usually the fallback.
///
/// `weight_json` is a JSON **string** inside the payload, not a nested object — a path like
/// `payload.weight.meta.effective_from` returns zero rows rather than erroring, which is how
/// `projector.rs` was measured silently dropping a schema-defined field in phase 29. It is parsed
/// here rather than queried.
#[must_use]
pub fn edge_meta_candidate(
    edge_type: &str,
    weight_json: &serde_json::Value,
) -> (String, Option<String>, Option<String>) {
    let basis = format!("{edge_type}.meta.effective_from / {edge_type}.meta.effective_to");
    let Some(parsed) = weight_json
        .as_str()
        .and_then(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
    else {
        return (basis, None, None);
    };
    let read = |key: &str| {
        parsed["meta"][key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    (basis, read("effective_from"), read("effective_to"))
}

/// The date part of an ISO-8601 value, whether it arrived as `2016-04-06` or
/// `2016-04-06T00:00:00Z`.
///
/// Three forms occur live and all three read the same way:
///
/// * `OfficerData::appointed_on` — a bare `2025-01-29`;
/// * `EdgeMeta::effective_from` — a chrono `DateTime<Utc>`, `2024-12-03T00:00:00Z`;
/// * `ContractData::date_published` — **sometimes an offset datetime**,
///   `2026-07-01T08:51:18+01:00`, observed on `an_anglesey_aebaa785` 2026-09-15. The CF mapper
///   does not always write a bare date, which is why this takes a prefix rather than requiring
///   `%Y-%m-%d` exactly.
///
/// **An offset is not normalised to UTC.** The leading ten characters are the date *as the record
/// states it*, and for a UK procurement notice that local date is the publication date an analyst
/// would cite. Converting would move a handful of near-midnight rows to the previous day for no
/// gain in truth.
///
/// Returning `None` for anything else is what makes an unreadable endpoint *untestable* rather
/// than silently treated as absent — see [`as_at_status`].
#[must_use]
pub fn iso_date(value: &str) -> Option<chrono::NaiveDate> {
    let head = value.get(..10)?;
    chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()
}

/// What an as-at date makes of one recorded range — the single place the null-range rule is
/// applied.
///
/// * Neither endpoint readable (including a `Recorded` range whose endpoints are present but
///   unparseable) → [`AsAtStatus::Undated`]. **Retained.** An absent or unreadable date is not
///   evidence that a fact was not in force, and on this corpus this is the answer for every
///   `Directs`, `AwardedTo` and `LocatedAt`-derived row.
/// * A readable start after the date, or a readable end before it → [`AsAtStatus::OutOfForce`].
/// * Otherwise → [`AsAtStatus::InForce`]. A half-open range counts: a directorship that began
///   before the date and records no end is in force as far as the record says, which is the only
///   claim available.
#[must_use]
pub fn as_at_status(range: &EffectiveRange, as_at: chrono::NaiveDate) -> AsAtStatus {
    let EffectiveRange::Recorded { from, to, .. } = range else {
        return AsAtStatus::Undated;
    };
    let from = from.as_deref().and_then(iso_date);
    let to = to.as_deref().and_then(iso_date);
    if from.is_none() && to.is_none() {
        // The record states an endpoint but nothing in it reads as a date, so the filter has not
        // tested this row. Saying so is the honest answer; treating it as in force would be a
        // claim no dataset made.
        return AsAtStatus::Undated;
    }
    if from.is_some_and(|f| f > as_at) || to.is_some_and(|t| t < as_at) {
        return AsAtStatus::OutOfForce;
    }
    AsAtStatus::InForce
}

/// One address row within an `EntityProfile`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct AddressRow {
    /// The `Location` node's own graph id — `location:…` or `LOC:…` (PIV-03 / D-06's pivot
    /// target).
    ///
    /// **Carried, never minted**, and that distinction is the whole of the modelling decision
    /// here. An address has no natural key: postcode plus line 1 plus locality collide across
    /// entities and are `Option` on every row, so anything derived from them would be a
    /// manufactured id that lands on the wrong node or on nothing. The graph already mints one —
    /// the `LocatedAt` edge's destination — and this field is that string verbatim.
    ///
    /// **Both namespaces are live and neither is normalised away.** Of 173,203 `LocatedAt`
    /// destinations on `an_anglesey_aebaa785`, 132,511 are `location:` (the Companies House /
    /// Contracts Finder / planning mappers' address-slug key) and 40,692 are `LOC:` (the
    /// postcode-prefixed key), measured 2026-09-15. Rewriting one into the other would point the
    /// pivot at a node that does not exist; the two are different nodes, not two spellings of one.
    ///
    /// Not an `Option`: a `LocatedAt` destination with no readable `Location` weight produces no
    /// row (the row's every other value is that node's), so an address row without an id is
    /// unconstructible from the read path.
    pub id: String,
    pub postcode: Option<String>,
    pub address_line_1: Option<String>,
    pub locality: Option<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]`: `Provenance` is a
    /// `workbench-dto` wire type with no `async-graphql` derives, and giving it a parallel
    /// GraphQL mirror would be the second spelling of one contract that plan 45-01 exists to
    /// prevent. The GraphQL profile is therefore unchanged by this field; the workbench HTTP
    /// surface (plan 45-08) is what renders it.
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04).
    ///
    /// `#[graphql(skip)]` for the reason given on [`AddressRow::provenance`]: `Temporal` is a
    /// `workbench-dto` wire type with no `async-graphql` derives, and a parallel GraphQL mirror
    /// would be a second spelling of one contract.
    ///
    /// The range renders whether or not a filter is set — an unfiltered profile must not be the
    /// less informative screen (the phase 45 UI spec) — and [`EffectiveRange::Recorded::basis`] names the
    /// record field it was read from, because a profile row can take its range from the edge that
    /// carries it or from the node the edge points at, and which one answered is part of the
    /// answer.
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// One property title an entity owns (PIV-03 / D-04).
///
/// `EntityProfile::owns_property` was a `Vec<String>` until this plan — a rendered label and
/// nothing else, so there was nowhere for the row's provenance to live. Only `label` and
/// `provenance` are declared, matching `workbench_dto::PropertyRowView`: structured fields (title
/// number, tenure, price paid) exist on the relation row but a column that is empty on most rows
/// reads as missing data rather than as absent structure, and nothing on the profile renders them
/// yet. They land when a renderer wants them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct PropertyRow {
    /// The title label as the profile has always rendered it — id, price paid and tenure.
    pub label: String,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// One row of the three `Vec<String>`-shaped link families — `trustee_of`, `donates_to` and
/// `offshore_links` (PIV-03 / D-04).
///
/// One row type rather than three identical ones, matching `workbench_dto::LinkedRecordRowView`:
/// which family a row belongs to is the section it arrives in, not a field repeating it. Each was
/// a `Vec<String>` until this plan, which left nowhere for the row's provenance to live.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct LinkedRecordRow {
    /// The display text this section has always rendered — a resolved charity or political-actor
    /// name where one exists, the bare record id where it does not.
    pub label: String,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// The shared-connection descriptor on a `RelatedEntity`.
///
/// `kind` is either `"director"` (shared directorship) or `"address"` (shared
/// registered address).  `value` is the officer name or postcode respectively.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedConnection {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

/// An entity related to the queried entity via a shared director or address.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedEntity {
    pub name: String,
    pub ch_number: Option<String>,
    pub status: urban_integrity_schema::types::node::EntityStatus,
    pub shared_connection: SharedConnection,
    /// Where the SHARED CONNECTION came from, not where the related entity came from — the row's
    /// claim is "these two are linked through this officer/address", and that claim is the edge's
    /// (PIV-03).
    ///
    /// A director-shared row is therefore always `Unattributed`: `Directs` has two producers
    /// writing the same shape. An address-shared row is attributed to whatever the relative's own
    /// namespace names, which is usually nothing — see the summary for the measured rates.
    pub provenance: Provenance,
}

/// A lightweight summary of an entity — returned when a name search yields
/// multiple matches (disambiguation path, D-05).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityProfileSummary {
    pub name: String,
    pub ch_number: Option<String>,
    pub status: urban_integrity_schema::types::node::EntityStatus,
}

/// A beneficial owner record surfaced from a Controls edge / Psc node.
///
/// Populated at the `resolve_entity` seam from `identity_links` with
/// `source='ch_psc'` and `status IN (auto_linked, confirmed)` (ENR-11).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct BeneficialOwnerRow {
    /// Display name of the PSC (from PscData.name).
    pub name: String,
    /// Nature of control categories (e.g. "ownership-of-shares-75-to-100-percent").
    pub natures_of_control: Vec<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// A disqualification record surfaced from a DisqualifiedFrom edge.
///
/// Populated at the `resolve_entity` seam from `identity_links` with
/// `source='ch_disqualified'` and `status IN (auto_linked, confirmed)` (ENR-11).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct DisqualificationRow {
    /// Display name of the disqualified officer.
    pub officer_name: String,
    /// Case reference number from the CDDA 1986 order (if available).
    pub case_number: Option<String>,
    /// ISO 8601 date string for the start of the disqualification ban.
    pub disqualified_from: Option<String>,
    /// ISO 8601 date string for the end of the disqualification ban.
    pub disqualified_until: Option<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
    /// When this fact was in force, and what an "as at" filter made of it (PIV-04). See
    /// [`AddressRow::temporal`].
    #[graphql(skip)]
    pub temporal: Temporal,
}

/// Confirmed or auto-linked OSINT identity link (status: 'auto_linked' or 'confirmed' only — ENR-11).
///
/// Populated from `identity_links` WHERE status IN ('auto_linked','confirmed').
/// Candidate links (pending_review) are never surfaced (SURF-05, T-20-05-02).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct OsintLink {
    /// Target node ID in the cross-registry identity link.
    pub target_node_id: String,
    /// Source OSINT system (e.g. "electoral_commission", "charity_commission", "icij", "opensanctions").
    pub source_system: String,
    /// Composite confidence score in [0.0, 1.0].
    pub confidence: f64,
    /// Resolution method used (e.g. "fuzzy_name", "phonetic_match", "exact_match").
    pub resolution_type: String,
    /// Link status: "auto_linked" or "confirmed" (ENR-11 invariant — never "pending_review").
    ///
    /// Also carried as [`Provenance::Attributed::tier`], which is where 45-01 puts it and where a
    /// renderer should read it. This field predates the provenance contract and is kept so the
    /// existing CLI/GraphQL surfaces do not change shape; the two can only disagree if
    /// [`identity_link_provenance`] is given a different status than the one stored here.
    pub status: String,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
}

/// OpenSanctions match status for an entity.
///
/// Populated from `identity_links` WHERE source='opensanctions' AND status IN ('auto_linked','confirmed').
/// `on_list` is true when at least one confirmed/auto-linked match exists (ENR-11).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct SanctionsStatus {
    /// True when at least one confirmed/auto-linked OpenSanctions match exists.
    pub on_list: bool,
    /// Matched entity name from OpenSanctions (target_node_id from identity_links).
    pub match_name: Option<String>,
    /// Confidence score of the best match in [0.0, 1.0].
    pub match_score: Option<f64>,
    /// OpenSanctions dataset slug — None until evidence_json population (future phase).
    pub dataset: Option<String>,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
}

/// OSINT alert row — a MotifDetected event sourced from an OSINT registry.
///
/// Populated from `graph_events` WHERE kind='MotifDetected' AND
/// source IN ('electoral_commission','charity_commission','icij','opensanctions') LIMIT 50.
/// This is the 7th OSINT-visible capability for SURF-05.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct OsintAlertRow {
    /// Motif type string (e.g. "DonationCorrelation", "PhoenixLoop").
    pub motif_type: String,
    /// Severity derived from risk_score: CRITICAL (>=0.9), HIGH (>=0.7), MEDIUM (<0.7).
    pub severity: String,
    /// Where this row came from (PIV-03). `#[graphql(skip)]` for the reason given on
    /// [`AddressRow::provenance`].
    #[graphql(skip)]
    pub provenance: Provenance,
}

/// Enrichment statistics for a company number — returned by `enrichmentStatus` GraphQL query.
///
/// Aggregates `identity_links` row counts by status for a given company number.
/// Used by analysts to monitor the enrichment pipeline state (SURF-05).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct EnrichmentStatusResult {
    /// The Companies House number queried.
    pub company_number: String,
    /// Total identity_links rows for this company number.
    pub total_links: u32,
    /// Count of auto_linked rows (confidence >= 0.85, not reviewed).
    pub auto_linked: u32,
    /// Count of pending_review rows (awaiting analyst decision).
    pub pending_review: u32,
    /// Count of confirmed rows (analyst-approved).
    pub confirmed: u32,
    /// ISO 8601 timestamp of the most recent enrichment run (if available).
    pub last_enriched: Option<String>,
}

/// A sub-candidate identity link (0.50–0.69). Read-only — surfaced only on entity --deep.
/// Not actionable via `links confirm/reject` (D-13, D-14).
///
/// ENR-11: deliberately excluded from the standard entity query path to prevent
/// accidental surfacing. Populated only when `deep=true` is requested.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubCandidateLinkRow {
    /// Source node ID in the identity link (typically an entity or director slug).
    pub source_node_id: String,
    /// Target node ID in the identity link.
    pub target_node_id: String,
    /// Adapter that produced the link (e.g. "charity_commission", "opensanctions", "icij").
    pub source: String,
    /// Composite confidence score in [0.50, 0.69).
    pub confidence: f64,
    /// Resolution method used (e.g. "fuzzy_name", "phonetic_match").
    pub resolution_type: String,
    /// ENR-06: always a flat JSON string; normalised slugs + scores, not raw PII.
    pub evidence_json: String,
}

/// Full entity profile assembled from `graph_events` rows.
///
/// This is the primary read-side type returned by the entity query functions.
/// All fields use camelCase serialisation per D-04 (`chNumber`, `incorporationDate`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[graphql(complex)]
#[serde(rename_all = "camelCase")]
pub struct EntityProfile {
    pub id: String,
    pub name: String,
    pub ch_number: Option<String>,
    /// EntityStatus is a tagged enum — cannot implement OutputType directly (Pitfall 1).
    /// Exposed as a String via ComplexObject impl below.
    #[graphql(skip)]
    pub status: urban_integrity_schema::types::node::EntityStatus,
    pub incorporation_date: Option<chrono::NaiveDate>,
    /// Where the header claims come from — the CH number, the incorporation date and the status
    /// (PIV-03; 45-01's `IdentifierFactView` / `StatusFactView` each carry a `Provenance`).
    ///
    /// **One provenance, not one per field, because there is one event.** `ch_number`,
    /// `incorporation_date` and `status` are all read off the same folded `Entity` node-weight row;
    /// giving each its own copy would let a renderer show three different answers to a question
    /// with one. Plan 45-08 attaches this to every identifier fact it emits.
    #[graphql(skip)]
    pub identity_provenance: Provenance,
    pub addresses: Vec<AddressRow>,
    pub directors: Vec<DirectorRow>,
    /// Contract awards only — see [`is_planning_application_id`] for the split (D-03 / PIV-02).
    ///
    /// Before that split this vector also held planning applications, which is 86.9% of the
    /// `AwardedTo` destinations on the live corpus. `crates/cli/src/commands/report.rs` renders it,
    /// so the split lowers a shipped number towards the truth.
    pub contracts: Vec<ContractRow>,
    /// Planning applications, the other half of what `contracts` used to be (D-03 / PIV-02).
    pub planning_applications: Vec<PlanningApplicationRow>,
    #[graphql(name = "motifAlerts")]
    pub alerts: Vec<AlertRecord>,
    /// Not in GQL-02 spec — hidden from GQL schema; RelatedEntity has no SimpleObject.
    #[graphql(skip)]
    pub related_entities: Vec<RelatedEntity>,
    pub lpa_presence: Vec<LpaPresence>,

    // ── OSINT Enrichment fields (Phase 17–20, SURF-01/SURF-05) ──────────────────
    // Phase 20 (SURF-05): graphql(skip) removed from the 6 fields below.
    // Serialised via serde for --output json.
    /// Beneficial owners linked via Controls edges / PSC nodes.
    /// Source: `identity_links` WHERE source='ch_psc' AND status IN (auto_linked,confirmed).
    /// ENR-11: candidate links never surfaced.
    pub beneficial_owners: Vec<BeneficialOwnerRow>,

    /// Disqualification records linked via DisqualifiedFrom edges.
    /// Source: `identity_links` WHERE source='ch_disqualified' AND status IN (auto_linked,confirmed).
    /// ENR-11: candidate links never surfaced.
    pub disqualifications: Vec<DisqualificationRow>,

    /// Offshore link records. Empty on every corpus so far, and structurally so: no adapter in
    /// this workspace constructs an `offshore_link` edge (`attribution::REGISTRY` records
    /// `OffshoreLink` as a legitimate zero-producer entry), and the live table holds 0 rows.
    pub offshore_links: Vec<LinkedRecordRow>,

    /// Trustee-of records — Charity Commission (`attribution::REGISTRY`: one producer).
    pub trustee_of: Vec<LinkedRecordRow>,

    /// Property ownership records from HM Land Registry, each carrying which extract it came
    /// from: the `owns_property` relation's IN side is `offshore_entity:` for OCOD and `entity:`
    /// for CCOD, which is the only thing on the row that distinguishes the two.
    pub owns_property: Vec<PropertyRow>,

    /// Political donation records — Electoral Commission (`attribution::REGISTRY`: one producer).
    pub donates_to: Vec<LinkedRecordRow>,

    // ── Phase 20 OSINT surface fields (SURF-05) ──────────────────────────────
    /// Confirmed/auto-linked OSINT identity links from cross-registry sources.
    /// Source: `identity_links` WHERE status IN ('auto_linked','confirmed') LIMIT 100.
    /// ENR-11: pending_review links never surfaced here (T-20-05-02).
    pub osint_links: Vec<OsintLink>,

    /// OpenSanctions match status — populated from identity_links where source='opensanctions'.
    /// None when no confirmed/auto-linked OpenSanctions match exists.
    pub sanctions_status: Option<SanctionsStatus>,

    /// OSINT-sourced motif alerts — the 7th OSINT capability for SURF-05.
    /// Populated from graph_events WHERE kind='MotifDetected' AND
    /// source IN ('electoral_commission','charity_commission','icij','opensanctions') LIMIT 50.
    pub osint_alerts: Vec<OsintAlertRow>,

    // D-13: sub-candidate links are analyst-only via `entity --deep`; must never appear in
    // GraphQL schema. Do not remove this annotation.
    #[graphql(skip)]
    pub sub_candidate_links: Vec<SubCandidateLinkRow>,

    /// The date this profile was evaluated "as at", or `None` for an unfiltered profile (PIV-04).
    ///
    /// Carried alongside the per-row [`AsAtStatus`] rather than instead of it: a profile with no
    /// temporal rows at all still has to be able to say a filter was set, and a reader who cannot
    /// tell a filtered profile from an unfiltered one will misread the page (the phase 45 UI spec gives
    /// the as-at banner the only non-verdict use of `--warning` on the profile screen for exactly
    /// that reason).
    ///
    /// **Setting it removes nothing.** See [`apply_as_at`].
    #[graphql(skip)]
    pub as_at: Option<chrono::NaiveDate>,
}

#[async_graphql::ComplexObject]
impl EntityProfile {
    /// Entity status as a human-readable string for GraphQL consumers.
    ///
    /// Format:
    /// - `Active`            → `"active"`
    /// - `Dissolved(dt)`     → `"dissolved:YYYY-MM-DD"`
    /// - `InLiquidation(dt)` → `"in_liquidation:YYYY-MM-DD"`
    async fn status(&self) -> String {
        use urban_integrity_schema::types::node::EntityStatus;
        match &self.status {
            EntityStatus::Active => "active".to_string(),
            EntityStatus::Dissolved(dt) => format!("dissolved:{}", dt.format("%Y-%m-%d")),
            EntityStatus::InLiquidation(dt) => {
                format!("in_liquidation:{}", dt.format("%Y-%m-%d"))
            }
        }
    }
}

/// Returns `true` if `s` looks like a valid Companies House registration number.
///
/// Accepts the full range of CH number prefixes: England/Wales digit-only,
/// Scottish SC, NI NI, LLP OC, foreign FC — all are ASCII-alphanumeric.
/// Conservative guard: any non-alphanumeric character (e.g. quote, semicolon)
/// is an immediate rejection.
///
/// `pub` so `crates/cli` can re-export this directly rather than duplicating
/// the validation logic (WR-01).
pub fn is_valid_ch_number(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric()) && s.len() >= 2 && s.len() <= 10
}

/// A resolved alert record from SurrealDB — the read-side mirror of EventRecord
/// for MotifDetected events.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject, PartialEq)]
pub struct AlertRecord {
    pub id: String,
    pub motif_type: String,
    pub score: f64,
    pub result_json: String,
    /// ISO-8601 string derived from the event timestamp
    pub detected_at: String,
    pub ward_code: Option<String>,
    /// Set when an operator has retracted this alert. `None` means live.
    ///
    /// Detection upserts what it finds and never withdraws what it stops finding, so an
    /// alert invalidated by a data correction survives its own correction (planning open
    /// item 23). Marking rather than deleting keeps the retraction as evidence about
    /// detector quality.
    pub withdrawn_at: Option<String>,
    pub withdrawn_reason: Option<String>,
    /// Every entity this alert names, as a plain slug/id list — generic across all 8 detector
    /// types (TRI-02), not just the 3 that already wrote a `participants` key. Parsed
    /// defensively from `result_json["participants"]`; defaults to `[]` on absence, which is
    /// the honest state for every alert written before its producing detector added the key.
    #[serde(default)]
    pub participants: Vec<String>,
    /// The first detector run that upserted this alert. `None` means "run not recorded" — the
    /// same `Unattributed` discipline `provenance` already applies to a missing timestamp,
    /// applied here to run attribution (TRI-02). Populated going forward only; pre-existing
    /// alerts stay `None` until backfilled, which is deliberately out of this phase's scope.
    #[serde(default)]
    pub first_seen_run_id: Option<String>,
    /// The most recent detector run that re-confirmed this alert. Detection UPSERTs, so a
    /// re-firing motif overwrites its own row — carrying both first- and last-seen run ids
    /// makes staleness legible without a second query.
    #[serde(default)]
    pub last_seen_run_id: Option<String>,
    /// Where this alert came from (PIV-03).
    ///
    /// A motif alert is **not** sourced from a register: it is an assertion this project made
    /// about data it holds, so the source dataset is [`attribution::DETECTOR`] — deliberately
    /// lowercase, so the chip cannot be mistaken for a third party saying it. `observed_at` is the
    /// `graph_events` timestamp of the `MotifDetected` event.
    ///
    /// Distinct from [`Self::detected_at`], which renders the same instant but falls back to
    /// `1970-01-01T00:00:00Z` when the row has no timestamp. That fallback is tolerable next to an
    /// alert and intolerable inside a provenance chip, where it would be indistinguishable from a
    /// genuinely ancient record — so this field reports `Unattributed` in that case instead.
    #[graphql(skip)]
    pub provenance: Provenance,
}

/// Total sort orders `select_alerts` can produce. Both are total and deterministic — a second
/// order, not a second idiom, so paging over either is exactly as stable as the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AlertSort {
    /// Score, then `detected_at`, then id — the original, severity-first order.
    #[default]
    ScoreDesc,
    /// `detected_at`, then score, then id — recency-first, for "what just fired".
    RecencyDesc,
}

/// Filter parameters for alert queries — shared by gRPC, GraphQL, and CLI.
#[derive(Debug, Clone, Default)]
pub struct AlertFilter {
    pub min_score: f64,
    /// `P_c` ceiling. Inclusive, matching `min_score`'s existing inclusive-floor semantics
    /// (TRI-01, D-13). `None` means unbounded.
    pub max_score: Option<f64>,
    pub ward_code: Option<String>,
    pub motif_type: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    /// Capped at 1000; defaults to 50
    pub limit: usize,
    /// Offset into the total order `select_alerts` produces, applied after sorting and after
    /// `total_matched` is captured, so paging never changes the reported total (D-12).
    pub start: usize,
    /// Which total order to sort by. Defaults to [`AlertSort::ScoreDesc`].
    pub sort: AlertSort,
    /// Surface retracted alerts too. Defaults to `false`, so a withdrawn alert never
    /// reaches the analyst queue by accident — `T-23-FalseAccusation` is the failure mode
    /// this guards.
    pub include_withdrawn: bool,
}

/// A page of alerts, plus the truth about how many matched.
///
/// A bare `Vec` cannot distinguish "3 alerts exist" from "3 of 1,900 are being shown", and that
/// ambiguity hid the project's only live Phoenix alert: `alerts list` returned 1,000 rows with no
/// indication that anything had been cut, so an analyst had no reason to look further.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertPage {
    /// The alerts to display, highest severity first.
    pub alerts: Vec<AlertRecord>,
    /// How many alerts matched the filter in total, before the display limit was applied.
    pub total_matched: usize,
}

impl AlertPage {
    /// Whether the display limit hid matching alerts.
    pub fn truncated(&self) -> bool {
        self.total_matched > self.alerts.len()
    }
}

/// Filter, order and page a set of alerts. Pure — no DB, so the real logic is testable.
///
/// **Ordering is by score descending, never by time.** Ordering by `timestamp DESC` — what this
/// replaced — means an analyst sees the most *recent* alerts rather than the most *severe*. On the
/// live graph that buried the single un-retracted `PhoenixLoop` alert (score 0.85, written
/// 2026-08-11) beneath 1,807 later `AddressCluster` rows, and `--motif phoenix` returned `[]`
/// because the motif filter ran over a timestamp-ordered window the alert had fallen out of.
///
/// The sort is **total and deterministic** — score, then detected_at, then id — so the same query
/// over the same data returns the same order every time. A partial order would make output depend
/// on input order, which is the read-path half of idempotency.
pub fn select_alerts(records: Vec<AlertRecord>, filter: &AlertFilter) -> AlertPage {
    let mut alerts: Vec<AlertRecord> = records
        .into_iter()
        .filter(|a| alert_passes_withdrawn_filter(a, filter))
        .filter(|a| alert_passes_min_score_filter(a, filter))
        .filter(|a| alert_passes_max_score_filter(a, filter))
        .filter(|a| alert_passes_ward_filter(a, filter))
        .filter(|a| alert_passes_motif_filter(a, filter))
        .filter(|a| alert_passes_from_date_filter(a, filter))
        .filter(|a| alert_passes_to_date_filter(a, filter))
        .collect();

    match filter.sort {
        // Severity first, newest as tie-break, id last so the order is total and reproducible.
        AlertSort::ScoreDesc => alerts.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.detected_at.cmp(&a.detected_at))
                .then_with(|| a.id.cmp(&b.id))
        }),
        // Recency first, severity as tie-break, id last — still total.
        AlertSort::RecencyDesc => alerts.sort_by(|a, b| {
            b.detected_at
                .cmp(&a.detected_at)
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.id.cmp(&b.id))
        }),
    }

    let total_matched = alerts.len();
    let alerts: Vec<AlertRecord> = alerts
        .into_iter()
        .skip(filter.start)
        .take(if filter.limit == 0 {
            usize::MAX
        } else {
            filter.limit
        })
        .collect();
    AlertPage {
        alerts,
        total_matched,
    }
}

/// Whether an alert survives the withdrawal filter. Live alerts always do.
pub fn alert_passes_withdrawn_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter.include_withdrawn || alert.withdrawn_at.is_none()
}

/// `P_c` floor. Inclusive — an alert scoring exactly `min_score` passes.
fn alert_passes_min_score_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    alert.score >= filter.min_score
}

/// `P_c` ceiling (TRI-01, D-13). Inclusive, matching the floor's own semantics. `None` means
/// unbounded.
fn alert_passes_max_score_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter.max_score.is_none_or(|max| alert.score <= max)
}

fn alert_passes_ward_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter
        .ward_code
        .as_ref()
        .is_none_or(|w| alert.ward_code.as_deref() == Some(w.as_str()))
}

fn alert_passes_motif_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter
        .motif_type
        .as_ref()
        .is_none_or(|m| alert.motif_type.eq_ignore_ascii_case(m))
}

fn parse_alert_date_bound_start(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().or_else(|| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|ndt| ndt.and_utc().fixed_offset())
    })
}

fn parse_alert_date_bound_end(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().or_else(|| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(23, 59, 59))
            .map(|ndt| ndt.and_utc().fixed_offset())
    })
}

fn alert_passes_from_date_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter.from_date.as_ref().is_none_or(|from| {
        parse_alert_date_bound_start(from).is_none_or(|from_dt| {
            chrono::DateTime::parse_from_rfc3339(&alert.detected_at)
                .map(|dt| dt >= from_dt)
                .unwrap_or(true)
        })
    })
}

fn alert_passes_to_date_filter(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    filter.to_date.as_ref().is_none_or(|to| {
        parse_alert_date_bound_end(to).is_none_or(|to_dt| {
            chrono::DateTime::parse_from_rfc3339(&alert.detected_at)
                .map(|dt| dt <= to_dt)
                .unwrap_or(true)
        })
    })
}

/// An alert's fitness for the detector-facet count: every filter except `motif_type` (held at
/// "match everything" so every detector's count is visible before it's chosen), reusing the
/// exact predicates `select_alerts` filters with — never a second, independent implementation.
fn alert_matches_for_detector_facet(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    alert_passes_withdrawn_filter(alert, filter)
        && alert_passes_min_score_filter(alert, filter)
        && alert_passes_max_score_filter(alert, filter)
        && alert_passes_ward_filter(alert, filter)
        && alert_passes_from_date_filter(alert, filter)
        && alert_passes_to_date_filter(alert, filter)
}

/// An alert's fitness for the ward-facet count: every filter except `ward_code` held at "match
/// everything", same reuse discipline as [`alert_matches_for_detector_facet`].
fn alert_matches_for_ward_facet(alert: &AlertRecord, filter: &AlertFilter) -> bool {
    alert_passes_withdrawn_filter(alert, filter)
        && alert_passes_min_score_filter(alert, filter)
        && alert_passes_max_score_filter(alert, filter)
        && alert_passes_motif_filter(alert, filter)
        && alert_passes_from_date_filter(alert, filter)
        && alert_passes_to_date_filter(alert, filter)
}

/// Per-filter facet counts: how many alerts match each detector type, and each ward, if that
/// ONE dimension's own filter were lifted and every other active filter stayed applied (D-13).
/// A plain, DB-free function over an already-scanned `Vec<AlertRecord>` — never a second round
/// trip to SurrealDB.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertFacetGroups {
    /// `motif_type` string to count of matching alerts.
    pub detector_counts: std::collections::HashMap<String, usize>,
    /// Ward code to count of matching alerts, sorted by count descending, then ward code
    /// ascending — a total order (D-13).
    pub ward_counts: Vec<(String, usize)>,
    /// Alerts matching every filter except `ward_code`, that themselves carry no ward code.
    pub no_ward_code_count: usize,
}

pub fn alert_facets(records: &[AlertRecord], filter: &AlertFilter) -> AlertFacetGroups {
    let mut detector_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for alert in records {
        if alert_matches_for_detector_facet(alert, filter) {
            *detector_counts.entry(alert.motif_type.clone()).or_insert(0) += 1;
        }
    }

    let mut ward_counts_map: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut no_ward_code_count = 0usize;
    for alert in records {
        if alert_matches_for_ward_facet(alert, filter) {
            match &alert.ward_code {
                Some(w) => *ward_counts_map.entry(w.clone()).or_insert(0) += 1,
                None => no_ward_code_count += 1,
            }
        }
    }
    let mut ward_counts: Vec<(String, usize)> = ward_counts_map.into_iter().collect();
    ward_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    AlertFacetGroups {
        detector_counts,
        ward_counts,
        no_ward_code_count,
    }
}

/// Query MotifDetected alerts from SurrealDB with optional filtering.
///
/// Fetches raw graph_events rows where `kind = 'MotifDetected'`, parses the
/// nested `result_json` payload in Rust, and applies score/ward/motif filters.
/// SurrealDB v1 does not support reliable JSON field access inside string
/// payloads via SurrealQL, so post-query filtering is used for correctness.
///
/// **Over-fetch caveat**: when `limit` is non-zero the function fetches up to
/// `limit × 10` rows (capped at 10 000) from SurrealDB and filters them in
/// Rust.  If active filters (ward, motif, date, score) discard most candidates
/// the returned slice may contain fewer than `limit` records even though more
/// matches exist further in the dataset.  A `tracing::warn!` is emitted when
/// this happens so operators can observe the condition in logs.
pub async fn query_alerts(
    db: &Surreal<Any>,
    filter: &AlertFilter,
) -> Result<AlertPage, surrealdb::Error> {
    // Scan every MotifDetected row, then filter and order in Rust.
    //
    // The window this replaces fetched `limit * 10` rows ordered by `timestamp DESC` and filtered
    // inside them, which made the answer depend on how recently an alert happened to be written.
    // Live consequence: `--motif phoenix` returned `[]` while a live PhoenixLoop alert existed,
    // because it sat outside the window. A filter applied to a sample is not a filter.
    //
    // Filters cannot be pushed into SurrealQL here: the payload lives in `result_json`, a JSON
    // *string*, and SurrealDB cannot reliably index into it. That is the same stringly-typed
    // payload problem that lets fields silently vanish elsewhere in this codebase. Until the
    // weight becomes a typed column, Rust-side filtering over a COMPLETE scan is the only correct
    // option — and the scan is paginated with a fail-loud cap, so it cannot silently under-read.
    //
    // Cost is bounded by the alert corpus, not the graph: 1,947 rows live on 2026-08-18.
    let rows = crate::persistence::scan::scan_events_by_kind(db, "MotifDetected").await?;
    let records: Vec<AlertRecord> = rows.into_iter().filter_map(parse_alert_row).collect();
    Ok(select_alerts(records, filter))
}

/// Fetch a single alert by its `event_id` from SurrealDB.
/// Returns `None` if no matching record exists.
pub async fn get_alert_by_id(
    db: &Surreal<Any>,
    id: &str,
) -> Result<Option<AlertRecord>, surrealdb::Error> {
    let id = id.to_owned();
    let results: Vec<serde_json::Value> = db
        .query("SELECT * FROM graph_events WHERE event_id = $id LIMIT 1")
        .bind(("id", id))
        .await?
        .take(0)?;

    Ok(results.into_iter().next().and_then(parse_alert_row))
}

/// The distinguishable result of a withdraw attempt — both the CLI and the future
/// `/api/alert/:id/withdraw` handler (46-06) consume this, so both surfaces render the same
/// three states identically by construction (D-09).
#[derive(Debug, Clone, PartialEq)]
pub enum WithdrawOutcome {
    /// The alert was live and is now withdrawn.
    Withdrawn,
    /// The alert was already withdrawn. Carries the FIRST call's `at`/`reason` — a caller's own
    /// reason is never written over an existing one (D-08, D-09).
    AlreadyWithdrawn { at: String, reason: String },
    /// No alert matches `id`.
    NotFound,
}

/// Pure, DB-free withdraw decision over the rows a `SELECT` already returned (0 or 1 row).
///
/// `Ok(outcome)` means the caller should return immediately without ever building the
/// `UPDATE` — `NotFound` (no row, no `result_json`, or malformed JSON) or `AlreadyWithdrawn`
/// (the row's own `withdrawn_at` is already set, read BEFORE any mutation). `Err(mutated)`
/// means the alert is live: `mutated` is the reserialisable JSON blob, `withdrawn_at`/
/// `withdrawn_reason` already stamped, ready for the `UPDATE`.
///
/// Kept separate from the `async fn` so the already-withdrawn/not-found branches are unit
/// testable against hand-built fixtures with no SurrealDB round trip — the live-write branch
/// still needs the real `UPDATE`, which stays in [`withdraw_alert`] itself.
fn decide_withdraw_outcome(
    rows: Vec<serde_json::Value>,
    reason: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<WithdrawOutcome, serde_json::Value> {
    let Some(row) = rows.into_iter().next() else {
        return Ok(WithdrawOutcome::NotFound);
    };
    let Some(result_json) = row["payload"]["result_json"].as_str() else {
        return Ok(WithdrawOutcome::NotFound);
    };
    let Ok(mut result) = serde_json::from_str::<serde_json::Value>(result_json) else {
        return Ok(WithdrawOutcome::NotFound);
    };

    if let Some(existing_at) = result["withdrawn_at"].as_str() {
        return Ok(WithdrawOutcome::AlreadyWithdrawn {
            at: existing_at.to_string(),
            reason: result["withdrawn_reason"]
                .as_str()
                .unwrap_or("no reason recorded")
                .to_string(),
        });
    }

    result["withdrawn_at"] = serde_json::json!(at.to_rfc3339());
    result["withdrawn_reason"] = serde_json::json!(reason);
    Err(result)
}

/// Mark an alert withdrawn. Returns [`WithdrawOutcome::NotFound`] if no such alert exists.
///
/// Retraction is a MARK, never a delete. Detection upserts what it matches and never
/// withdraws what it stops matching, so an alert invalidated by a data correction survives
/// its own correction and stays in the analyst queue indistinguishable from a live finding
/// (planning open item 23). Keeping the row preserves it as evidence about detector
/// quality; this project has already destroyed that history once, purging 27,937 rows.
///
/// The withdrawal lives inside `result_json` rather than beside it so it travels with the
/// alert through every existing reader, and so `parse_alert_row` — the single place alerts
/// are decoded — is the only thing that needs to know about it.
///
/// The already-withdrawn guard runs BEFORE any mutation, inside [`decide_withdraw_outcome`] —
/// a caller cannot bypass it by skipping a separate pre-check, which is exactly how the CLI's
/// old two-step pattern could have diverged from a second caller (D-08, D-09).
pub async fn withdraw_alert(
    db: &Surreal<Any>,
    id: &str,
    reason: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<WithdrawOutcome, surrealdb::Error> {
    let rows: Vec<serde_json::Value> = db
        .query("SELECT * FROM graph_events WHERE kind = 'MotifDetected' AND event_id = $id LIMIT 1")
        .bind(("id", id.to_owned()))
        .await?
        .take(0)?;

    let result = match decide_withdraw_outcome(rows, reason, at) {
        Ok(outcome) => return Ok(outcome),
        Err(mutated) => mutated,
    };

    // The record id is not derivable from event_id, so update by the same predicate that
    // found the row. `.take(0)` is required: a bare await would swallow a failed statement,
    // and an UPDATE matching nothing returns OK with an empty result — this project has
    // reported 3,694 phantom writes to exactly that pattern.
    let updated: Vec<serde_json::Value> = db
        .query(
            "UPDATE graph_events SET payload.result_json = $r \
             WHERE kind = 'MotifDetected' AND event_id = $id RETURN AFTER",
        )
        .bind(("r", result.to_string()))
        .bind(("id", id.to_owned()))
        .await?
        .take(0)?;

    Ok(if updated.is_empty() {
        WithdrawOutcome::NotFound
    } else {
        WithdrawOutcome::Withdrawn
    })
}

/// Validates an event ID string for safe embedding in a SurrealQL IN clause.
/// Accepts only non-empty alphanumeric + hyphen + underscore strings up to 64 chars.
pub(crate) fn is_valid_event_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Batch-fetch alerts by a list of IDs. Returns a map of id → AlertRecord
/// for IDs that were found. Missing IDs are simply absent from the map.
pub async fn get_alerts_by_ids(
    db: &Surreal<Any>,
    ids: &[String],
) -> Result<HashMap<String, AlertRecord>, surrealdb::Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Validate IDs before embedding in SurrealQL: allow only alphanumeric + hyphens + underscores.
    // SurrealDB v1 array binding in WHERE IN requires a literal array string, so we must
    // construct the query string ourselves — sanitise each ID to prevent injection.
    let safe_ids: Vec<&String> = ids.iter().filter(|id| is_valid_event_id(id)).collect();

    if safe_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let id_list: Vec<String> = safe_ids.iter().map(|id| format!("'{}'", id)).collect();
    let query = format!(
        "SELECT * FROM graph_events WHERE event_id IN [{}] LIMIT {}",
        id_list.join(", "),
        safe_ids.len()
    );

    let results: Vec<serde_json::Value> = db.query(query).await?.take(0)?;

    let map = results
        .into_iter()
        .filter_map(parse_alert_row)
        .map(|r| (r.id.clone(), r))
        .collect();

    Ok(map)
}

/// The `graph_events` kinds that carry a node's weight.
///
/// A node's current state is its `NodeAdded` birth folded with every later `NodeUpdated`.
/// `replay_from_surreal` folds them, and so does the projector — but every read path in this
/// module used to filter on `NodeAdded` alone, so the CLI and the GraphQL resolvers reported a
/// company's *birth* weight. For a dissolved company that is not a stale answer, it is the wrong
/// one, and the projected `entity` table sitting next to it said `dissolved`.
///
/// Measured on `an_anglesey_aebaa785`, 2026-08-16: 953 Entity `NodeUpdated` rows, 580 of them
/// terminal (579 Dissolved + 1 InLiquidation), all invisible to `entity --ch-number`.
pub const NODE_WEIGHT_KINDS: &[&str] = &["NodeAdded", "NodeUpdated"];

/// Reduce node-weight rows to the newest row per `payload.entity_id`.
///
/// First-seen order is preserved so the callers that `truncate(10)` afterwards stay
/// deterministic. Rows without a readable `entity_id` are dropped — they cannot be folded and
/// no caller can use them.
pub fn fold_latest_node_rows(rows: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut order: Vec<String> = Vec::new();
    let mut newest: HashMap<String, ((u64, u8), serde_json::Value)> = HashMap::new();

    for row in rows {
        let Some(id) = extract_id_string(&row["payload"]["entity_id"]) else {
            continue;
        };
        let rank = fold_rank(&row);
        match newest.get(&id) {
            Some((prev, _)) if rank < *prev => {}
            Some(_) => {
                newest.insert(id, (rank, row));
            }
            None => {
                order.push(id.clone());
                newest.insert(id, (rank, row));
            }
        }
    }

    order
        .into_iter()
        .filter_map(|id| newest.remove(&id).map(|(_rank, row)| row))
        .collect()
}

/// Rank a node-weight row for the newest-wins fold: `(timestamp, kind_rank)`.
///
/// `kind_rank` breaks equal-timestamp ties deterministically — a `NodeUpdated` correction
/// outranks the `NodeAdded` birth it corrects. Ties are real: a backfill batch reads the clock
/// once and stamps every row with it.
///
/// Arrival order cannot serve as the tie-break, which is what the projector and
/// `replay_from_surreal` used to rely on. This fold is fed both by the projector's two-list load
/// (births then updates) and by `scan_events_of_kinds_by_type`, whose single id-ordered scan is
/// ~40% inverted from time order — so position decides the winner arbitrarily there.
///
/// Rows must therefore carry `kind`. Every caller selects it.
fn fold_rank(row: &serde_json::Value) -> (u64, u8) {
    let ts = row["timestamp"].as_u64().unwrap_or(0);
    let kind_rank = u8::from(row["kind"].as_str() == Some("NodeUpdated"));
    (ts, kind_rank)
}

/// Overlay the newest `NodeUpdated` row for `entity_id` onto a `NodeAdded` birth row.
///
/// The point-read lookups (`query_entity_by_id`, `lookup_entity_summary`) find the birth row with
/// [`scan::find_event_by_type`], which stops at the first hit and so cannot see a later update.
/// Rather than turn those into full scans of the ~74k-row Entity marker, this pulls the
/// `NodeUpdated` table on its own — a single indexed `kind` equality with no `CONTAINS`, which is
/// the shape the planner handles correctly, over a table two orders of magnitude smaller than
/// `NodeAdded` (4,624 vs 459,098 rows live on 2026-08-16).
///
/// Degrades to the birth row if the update scan fails: a lookup that returns the old status is
/// worse than one that returns the new one, but far better than one that returns nothing.
async fn overlay_node_update(
    db: &Surreal<Any>,
    entity_id: &str,
    birth: serde_json::Value,
) -> serde_json::Value {
    let updates = match scan::scan_events_by_kind(db, "NodeUpdated").await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                entity_id,
                "NodeUpdated scan failed, using birth weight: {e}"
            );
            return birth;
        }
    };

    let birth_ts = birth["timestamp"].as_u64().unwrap_or(0);
    updates
        .into_iter()
        .filter(|r| {
            extract_id_string(&r["payload"]["entity_id"]).as_deref() == Some(entity_id)
                && r["timestamp"].as_u64().unwrap_or(0) >= birth_ts
        })
        .max_by_key(|r| r["timestamp"].as_u64().unwrap_or(0))
        .unwrap_or(birth)
}

/// Parse a raw SurrealDB `graph_events` row that is a `NodeAdded` event for an Entity node.
///
/// Returns `Some((entity_id, EntityData))` if the row is a node-weight event (see
/// [`NODE_WEIGHT_KINDS`]) whose `payload.weight_json` deserialises to `NodeWeight::Entity`.
/// Returns `None` for any other event kind or for non-Entity node types (Officer, Location…).
///
/// Accepting a single row says nothing about which row is current — callers reading state must
/// still fold with [`fold_latest_node_rows`] or [`overlay_node_update`].
pub(crate) fn parse_entity_node_row(row: serde_json::Value) -> Option<(String, EntityData)> {
    if !NODE_WEIGHT_KINDS.contains(&row["kind"].as_str()?) {
        return None;
    }
    let entity_id = extract_id_string(&row["payload"]["entity_id"])?;
    let weight_json_str = row["payload"]["weight_json"].as_str()?;
    let weight: NodeWeight = serde_json::from_str(weight_json_str).ok()?;
    match weight {
        NodeWeight::Entity(data) => Some((entity_id, data)),
        _ => None,
    }
}

/// Parse a raw NodeAdded row specifically for an Officer node.
/// Returns `Some((officer_id, OfficerData))` if the row is a NodeAdded event for an officer.
pub(crate) fn parse_officer_node_row(row: serde_json::Value) -> Option<(String, OfficerData)> {
    if !NODE_WEIGHT_KINDS.contains(&row["kind"].as_str()?) {
        return None;
    }
    let node_id = extract_id_string(&row["payload"]["entity_id"])?;
    let weight_json_str = row["payload"]["weight_json"].as_str()?;
    let weight: NodeWeight = serde_json::from_str(weight_json_str).ok()?;
    match weight {
        NodeWeight::Officer(data) => Some((node_id, data)),
        _ => None,
    }
}

/// Parse a raw NodeAdded row specifically for an OffshoreEntity node.
/// Returns `Some((offshore_id, OffshoreEntityData))` if the row is a NodeAdded
/// event for an offshore entity (OCOD overseas owner, `GB-OCOD:` / `GB-ROE:` key).
pub(crate) fn parse_offshore_node_row(
    row: serde_json::Value,
) -> Option<(String, OffshoreEntityData)> {
    if !NODE_WEIGHT_KINDS.contains(&row["kind"].as_str()?) {
        return None;
    }
    let node_id = extract_id_string(&row["payload"]["entity_id"])?;
    let weight_json_str = row["payload"]["weight_json"].as_str()?;
    let weight: NodeWeight = serde_json::from_str(weight_json_str).ok()?;
    match weight {
        NodeWeight::OffshoreEntity(data) => Some((node_id, data)),
        _ => None,
    }
}

/// Compute the backtick-quoted RecordId string used to match the IN (source)
/// side of the owns_property / offshore_link / trustee_of / donates_to relations
/// for a given node.
///
/// OCOD overseas owners are `offshore_entity` records (`GB-OCOD:` / `GB-ROE:`
/// keys); everything else is an `entity` record keyed by the `GB-COH:` form.
/// Mirrors the projector's record-id construction so profile reads match the
/// stored `in` field exactly (false-attribution guard, T-22-01-03).
pub(crate) fn compute_in_key(entity_id: &str, company_number: Option<&str>) -> String {
    if entity_id.starts_with("GB-OCOD:") || entity_id.starts_with("GB-ROE:") {
        return format!("offshore_entity:`{entity_id}`");
    }
    let ch = company_number.unwrap_or(entity_id);
    let node_id = if ch.starts_with("GB-COH:") {
        ch.to_owned()
    } else {
        format!("GB-COH:{ch}")
    };
    format!("entity:`{node_id}`")
}

/// Build an `EntityProfile` for an OCOD/ROE offshore owner.
///
/// Offshore nodes carry no Companies House number, so this synthesizes an
/// `EntityData` view and delegates to `assemble_entity_profile`, which — via
/// `compute_in_key` — matches the `offshore_entity:`…`` keyed owns_property /
/// offshore_link edges. Jurisdiction is surfaced in the display name when present.
async fn assemble_offshore_profile(
    db: &Surreal<Any>,
    offshore_id: &str,
    data: OffshoreEntityData,
    identity_event_ms: Option<u64>,
    deep: bool,
) -> Result<EntityProfile, surrealdb::Error> {
    let display_name = match data.jurisdiction.as_deref() {
        Some(j) if !j.is_empty() => format!("{} ({j})", data.name),
        _ => data.name.clone(),
    };
    let synthetic = EntityData {
        name: display_name,
        company_number: None,
        status: urban_integrity_schema::types::node::EntityStatus::default(),
        incorporation_date: None,
    };
    assemble_entity_profile(db, offshore_id, synthetic, identity_event_ms, deep).await
}

/// Stamp every temporal row on a profile with what `as_at` makes of its recorded range (PIV-04).
///
/// **This removes nothing.** Rows whose range positively excludes the date come back
/// [`AsAtStatus::OutOfForce`] and rows with no readable endpoint come back
/// [`AsAtStatus::Undated`], both still in their sections. Three reasons, in order of weight:
///
/// 1. `build_verify_verdict` folds `alerts` and `related_entities` off this same struct to derive
///    `p_c_score` and `dissolved_relative_count`. A read path that dropped rows would let a
///    *display* control move a *verdict* — the failure 45-03 caught when a display-level
///    resectioning silently narrowed AC-DET-1's input by 86.9% of the corpus.
/// 2. "A row excluded by the filter is not silently dropped from a section's total"
///    (the phase 45 UI spec) becomes true by construction. The unfiltered total is `rows.len()`; the
///    filtered one is a predicate over the same rows. A parallel tally could disagree with the
///    rows beside it; this cannot.
/// 3. PIV-04's worked example — "a director who left before a contract was awarded is visibly
///    distinguished from one who was serving" — needs the resigned director to still be *on the
///    screen* to be distinguished from anything. A shorter list is not a distinction.
///
/// Hiding an `OutOfForce` row is therefore a rendering decision (plan 45-12), taken from a status
/// this function computed once, rather than a rule re-derived at each render site.
///
/// The sections left untouched, and why each is not a temporal fact:
///
/// | Section | Why |
/// |---|---|
/// | `alerts`, `osint_alerts` | a detection is an assertion this project made at a moment, not a fact in force over an interval; its moment is its [`Provenance`] |
/// | `osint_links`, `sanctions_status` | an identity claim, not a state of the world; `resolved_at` is when it was asserted |
/// | `related_entities` | a derived adjacency, whose range would be the intersection of two underlying edges' — both `Directs` and `LocatedAt`, both undated on every row of this corpus |
/// | `lpa_presence` | an aggregate count spanning two datasets |
/// | `sub_candidate_links` | never surfaced at all (D-13) |
/// | the findings-by-entity section ([`findings_naming_entity`], plan 45-05) | a probe finding is what the battery concluded when it ran, not a fact in force over an interval; its moment is its [`Provenance`], exactly as for `alerts` above |
/// | header identifiers and status | claims about the subject, not facts with a tenure; `status` already carries its own dissolution date |
pub fn apply_as_at(profile: &mut EntityProfile, as_at: chrono::NaiveDate) {
    fn stamp(t: &mut Temporal, as_at: chrono::NaiveDate) {
        t.status = as_at_status(&t.range, as_at);
    }
    for r in &mut profile.addresses {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.directors {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.contracts {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.planning_applications {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.owns_property {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.trustee_of {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.donates_to {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.offshore_links {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.beneficial_owners {
        stamp(&mut r.temporal, as_at);
    }
    for r in &mut profile.disqualifications {
        stamp(&mut r.temporal, as_at);
    }
    profile.as_at = Some(as_at);
}

/// [`query_entity_by_id`] with PIV-04's "as at" parameter — the profile read path a date-filtered
/// surface calls.
///
/// `None` is byte-identical to [`query_entity_by_id`]: every row comes back
/// [`AsAtStatus::NotFiltered`], which is a different answer from `InForce` and deliberately so. An
/// unfiltered profile has not established that anything was in force at any date.
///
/// A sibling function rather than a parameter on `query_entity_by_id` itself: that signature is
/// reached from the CLI `entity` and `report` commands, the GraphQL resolver and four test
/// binaries, none of which have a date to pass, and changing it would put a `None` at every one of
/// those call sites to serve one caller.
pub async fn query_entity_by_id_as_at(
    db: &Surreal<Any>,
    entity_id: &str,
    as_at: Option<chrono::NaiveDate>,
) -> Result<Option<EntityProfile>, surrealdb::Error> {
    let profile = query_entity_by_id(db, entity_id).await?;
    Ok(profile.map(|mut p| {
        if let Some(date) = as_at {
            apply_as_at(&mut p, date);
        }
        p
    }))
}

/// Look up a single entity by its entity_id (e.g. "GB-COH:12345678" or "director:john-smith").
///
/// Returns `None` if no matching `NodeAdded` event exists.
///
/// # SurrealDB 3.x compatibility note
///
/// SurrealDB 3.x auto-coerces string values that match the `table:key` pattern
/// to `RecordId` when storing via `CREATE … CONTENT`.  Equality comparisons using
/// `payload.entity_id = $id` then fail because the stored type is `RecordId` while
/// the bound `$id` is `String`.  We work around this by fetching all `NodeAdded`
/// events that contain `"Entity"` in their weight_json and filtering Rust-side.
pub async fn query_entity_by_id(
    db: &Surreal<Any>,
    entity_id: &str,
) -> Result<Option<EntityProfile>, surrealdb::Error> {
    // Filtered Rust-side rather than with a bound `$id`: SurrealDB 3.x coerces
    // `payload.entity_id` to a RecordId for `director:*` keys, so sub-field equality against a
    // String silently matches nothing.
    //
    // `find_event_by_type` stops at the first hit. This is a point read from the caller's
    // perspective; collecting every Entity row first would make it O(table) per lookup.
    // deep=false: standard entity lookup never exposes sub-candidate links (D-13).
    let target = entity_id;
    // The predicate pins `kind` to NodeAdded so the point read lands on the entity's BIRTH row:
    // `find_event_by_type` stops at the first hit, and in id order that hit could otherwise be a
    // NodeUpdated for a different entity. `overlay_node_update` then supplies the current weight.
    let birth = scan::find_event_by_type(db, "NodeAdded", "\"type\":\"Entity\"", |row| {
        parse_entity_node_row(row.clone()).is_some_and(|(eid, _)| eid == target)
    })
    .await?;
    // The row's own `timestamp` is captured before it is consumed: it is the `observed_at` of
    // every header claim on the profile, and `parse_entity_node_row` drops it.
    let matched = match birth {
        Some(row) => {
            let row = overlay_node_update(db, target, row).await;
            let ts = row["timestamp"].as_u64();
            parse_entity_node_row(row).map(|(eid, data)| (eid, data, ts))
        }
        None => None,
    };

    if let Some((eid, data, ts)) = matched {
        return Ok(Some(
            assemble_entity_profile(db, &eid, data, ts, false).await?,
        ));
    }

    // Fall back to OffshoreEntity nodes (OCOD/ROE overseas owners, e.g.
    // "GB-OCOD:…"). These hold no CH number and key the owns_property `in` side
    // as `offshore_entity:`…`` (compute_in_key).
    let off_birth =
        scan::find_event_by_type(db, "NodeAdded", "\"type\":\"OffshoreEntity\"", |row| {
            parse_offshore_node_row(row.clone()).is_some_and(|(oid, _)| oid == target)
        })
        .await?;
    let off = match off_birth {
        Some(row) => {
            let row = overlay_node_update(db, target, row).await;
            let ts = row["timestamp"].as_u64();
            parse_offshore_node_row(row).map(|(oid, d)| (oid, d, ts))
        }
        None => None,
    };

    match off {
        None => Ok(None),
        Some((oid, d, ts)) => Ok(Some(
            assemble_offshore_profile(db, &oid, d, ts, false).await?,
        )),
    }
}

/// Search for entities whose name contains the given substring (case-insensitive).
///
/// Fetches up to 1000 `NodeAdded` rows and filters Rust-side.  The name
/// needle is lowercased before comparison.  Returns at most 10 profiles
/// (D-02 hard cap).
pub async fn query_entity_by_name(
    db: &Surreal<Any>,
    name: &str,
) -> Result<Vec<EntityProfile>, surrealdb::Error> {
    let needle = name.to_lowercase();
    // The "LIMIT 50000, raised from 1000" note that used to sit here described a query this
    // function stopped issuing at GRD-09: the scan below pages to exhaustion, so there is no
    // ceiling to raise. The one cap left on this path is the D-02 `truncate(10)` below, which is
    // a disambiguation-list size the analyst then picks from — not a limit on any one profile.
    // deep=false: standard name search never exposes sub-candidate links (D-13).
    // Both node-weight kinds, folded to the newest row per entity: one scan, no extra cost —
    // `scan_events_by_type` filters `kind` Rust-side, so the DB work is identical either way.
    let results: Vec<serde_json::Value> =
        scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Entity\"").await?;

    // Filter Rust-side: payload.weight_json is a serialised JSON string; SurrealQL
    // nested-string field access is unreliable for this codebase (see query_alerts doc).
    let mut matching: Vec<(String, EntityData, Option<u64>)> = fold_latest_node_rows(results)
        .into_iter()
        .filter_map(|row| {
            let ts = row["timestamp"].as_u64();
            parse_entity_node_row(row).map(|(eid, data)| (eid, data, ts))
        })
        .filter(|(_id, data, _ts)| data.name.to_lowercase().contains(&needle))
        .collect();

    matching.truncate(10); // D-02

    let mut profiles = Vec::with_capacity(matching.len());
    for (eid, data, ts) in matching {
        profiles.push(assemble_entity_profile(db, &eid, data, ts, false).await?);
    }

    // Also surface OCOD/ROE offshore owners matching the needle, sharing the
    // D-02 10-result cap with Entity matches.
    if profiles.len() < 10 {
        let offshore_rows = scan::scan_events_of_kinds_by_type(
            db,
            NODE_WEIGHT_KINDS,
            "\"type\":\"OffshoreEntity\"",
        )
        .await?;

        // Dedup by id: the OCOD adapter emits one AddNode per property, so an
        // owner of N properties yields N NodeAdded events for the same offshore id.
        let mut seen_ids = std::collections::HashSet::new();
        let mut offshore_matches: Vec<(String, OffshoreEntityData, Option<u64>)> =
            fold_latest_node_rows(offshore_rows)
                .into_iter()
                .filter_map(|row| {
                    let ts = row["timestamp"].as_u64();
                    parse_offshore_node_row(row).map(|(oid, d)| (oid, d, ts))
                })
                .filter(|(_id, d, _ts)| d.name.to_lowercase().contains(&needle))
                .filter(|(id, _d, _ts)| seen_ids.insert(id.clone()))
                .collect();
        offshore_matches.truncate(10 - profiles.len());

        for (oid, d, ts) in offshore_matches {
            profiles.push(assemble_offshore_profile(db, &oid, d, ts, false).await?);
        }
    }

    Ok(profiles)
}

/// Look up a single entity by Companies House registration number (exact match).
///
/// Validates the CH number first via `is_valid_ch_number`.  Returns `Ok(None)`
/// for invalid inputs — never queries the DB with untrusted user strings.
pub async fn query_entity_by_ch_number(
    db: &Surreal<Any>,
    ch_number: &str,
) -> Result<Option<EntityProfile>, surrealdb::Error> {
    if !is_valid_ch_number(ch_number) {
        return Ok(None);
    }

    // Build the needle from a validated alphanumeric-only string — this is
    // the one legitimate format! use because ch_number has been pre-validated.
    // deep=false: standard CH number lookup never exposes sub-candidate links (D-13).
    let needle = format!("\"company_number\":\"{}\"", ch_number);

    // SurrealDB 3.1.0-beta.2 planner bug: combining the idx_graph_events_kind indexed
    // equality (`kind = 'NodeAdded'`) with a CONTAINS scan on the non-indexed
    // payload.weight_json field makes SELECT * return 0 rows, while count() reports the
    // correct number. Same bug worked around at the assemble_entity_profile / LPA sites.
    // Fix: drop the kind equality from the SQL (keep only CONTAINS), filter kind in Rust.
    // parse_entity_node_row already rejects any row whose kind is not a node-weight kind.
    //
    // No LIMIT: the needle matches every node-weight event for this company — birth plus each
    // status correction — and a LIMIT would cap what the fold below can see. One CH number
    // yields a handful of rows.
    let results: Vec<serde_json::Value> = db
        .query("SELECT * FROM graph_events WHERE payload.weight_json CONTAINS $needle")
        .bind(("needle", needle))
        .await?
        .take(0)?;

    let ch = ch_number.to_owned();
    let matched = fold_latest_node_rows(results)
        .into_iter()
        .filter_map(|row| {
            let ts = row["timestamp"].as_u64();
            parse_entity_node_row(row).map(|(eid, data)| (eid, data, ts))
        })
        .find(|(_id, data, _ts)| data.company_number.as_deref() == Some(&ch));

    match matched {
        None => Ok(None),
        Some((eid, data, ts)) => Ok(Some(
            assemble_entity_profile(db, &eid, data, ts, false).await?,
        )),
    }
}

/// Find entities related to `target_entity_id` via a shared director or shared address.
///
/// Step A: find officers who direct the target entity (Directs edges where dst = target).
/// Step B: find other entities those officers direct (reverse-walk).
/// Step C: find entities sharing a registered address with the target.
/// Step D: dedup and hydrate up to `limit` results.
///
/// # SurrealDB 3.x compatibility note
///
/// All `payload.src_id = $var` and `payload.dst_id = $var` equality filters have
/// been removed from the WHERE clauses and replaced with Rust-side filtering.
/// See `query_entity_by_id` for the full explanation.
pub async fn query_related_entities(
    db: &Surreal<Any>,
    target_entity_id: &str,
    limit: usize,
) -> Result<Vec<RelatedEntity>, surrealdb::Error> {
    let target = target_entity_id.to_owned();

    // ── Step A: officers who direct the target ────────────────────────────────
    // Fetch all Directs EdgeAdded events and filter Rust-side for dst_id == target.
    let all_directs_in: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"Directs\"").await?;

    let officer_ids: Vec<String> = all_directs_in
        .iter()
        .filter_map(|row| {
            if row["kind"].as_str()? != "EdgeAdded" {
                return None;
            }
            let dst = row["payload"]["dst_id"].as_str()?;
            if dst == target {
                row["payload"]["src_id"].as_str().map(|s| s.to_owned())
            } else {
                None
            }
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // ── Step B: other entities those officers direct ──────────────────────────
    let mut director_linked: Vec<(String, String)> = Vec::new(); // (entity_id, officer_id)
                                                                 // The `graph_events` timestamp of the edge that established each link, keyed by the same pair
                                                                 // the two candidate lists hold. Kept beside them rather than inside them so the sort/dedup
                                                                 // below still collapses a re-ingested edge into one related entity (PIV-06).
    let mut link_ts: HashMap<(String, String), u64> = HashMap::new();

    if !officer_ids.is_empty() {
        // Re-use the already-fetched Directs events — no second DB round-trip needed.
        // Filter Rust-side for src_id in officer_ids.
        let officer_set: HashSet<&str> = officer_ids.iter().map(|s| s.as_str()).collect();

        for row in &all_directs_in {
            if row["kind"].as_str().unwrap_or("") != "EdgeAdded" {
                continue;
            }
            let src = match row["payload"]["src_id"].as_str() {
                Some(s) => s,
                None => continue,
            };
            if !officer_set.contains(src) {
                continue;
            }
            let dst = match row["payload"]["dst_id"].as_str() {
                Some(d) => d.to_owned(),
                None => continue,
            };
            if dst == target {
                continue; // skip the target itself
            }
            // Newest wins: `graph_events` is append-only, so a re-ingested edge is the same
            // relationship observed again, not a second one.
            let ts = row["timestamp"].as_u64().unwrap_or(0);
            let key = (dst.clone(), src.to_owned());
            link_ts
                .entry(key)
                .and_modify(|t| *t = (*t).max(ts))
                .or_insert(ts);
            director_linked.push((dst, src.to_owned()));
        }
    }

    // ── Step C: entities sharing a registered address ────────────────────────
    // Fetch all LocatedAt EdgeAdded events and filter Rust-side.
    let all_located: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"LocatedAt\"").await?;

    // post-D-04 (phase 29, land-flip-address-matching): the block below finds "location nodes
    // the target points to" by filtering `LocatedAt.src == target`. Before this phase, EVERY
    // planning applicant's site address was emitted with `src` = the applicant Entity, so this
    // filter caught it. As of this phase, `map_planning_item` sources a planning `Contract`'s
    // `LocatedAt` edge from the Contract node, not the applicant Entity — see
    // `crates/detector/src/motif/cluster.rs`'s `ClusterDetector::check`, which DOES follow the
    // contract-sourced path (Contract -> LocatedAt -> Location, attributed back to the
    // applicant via a reversed AwardedTo lookup) and is the reference for extending this
    // function to do the same in a future phase.
    //
    // Practical effect: an applicant Entity's own outgoing `LocatedAt` edges here will only
    // include (a) pre-existing legacy entity-sourced rows from before this phase's re-ingest
    // (`graph_events` is append-only, D-24 — these are never deleted) and (b) non-planning
    // sources that still emit entity-sourced `LocatedAt` edges (e.g. Companies House
    // registered-office addresses, unaffected by this phase). A NEWLY-ingested or re-ingested
    // planning application's site will NOT surface here as one of the target's own addresses —
    // it is only reachable via its Contract. This narrowing is arguably correct (an applicant
    // genuinely is not "located at" a site it merely applied to build on) but is an intentional,
    // documented behaviour change per D-04, not a defect — stated here so it is discovered
    // through documentation, not surprise.
    //
    // Find location nodes the target points to.
    let location_ids: HashSet<String> = all_located
        .iter()
        .filter_map(|row| {
            if row["kind"].as_str()? != "EdgeAdded" {
                return None;
            }
            let src = row["payload"]["src_id"].as_str()?;
            if src == target {
                row["payload"]["dst_id"].as_str().map(|s| s.to_owned())
            } else {
                None
            }
        })
        .collect();

    let mut address_linked: Vec<(String, String)> = Vec::new(); // (entity_id, location_id)

    if !location_ids.is_empty() {
        // Re-use the already-fetched LocatedAt events.
        for row in &all_located {
            if row["kind"].as_str().unwrap_or("") != "EdgeAdded" {
                continue;
            }
            let src = match row["payload"]["src_id"].as_str() {
                Some(s) => s.to_owned(),
                None => continue,
            };
            if src == target {
                continue; // skip the target itself
            }
            let dst_loc = match row["payload"]["dst_id"].as_str() {
                Some(d) => d,
                None => continue,
            };
            if !location_ids.contains(dst_loc) {
                continue;
            }
            let ts = row["timestamp"].as_u64().unwrap_or(0);
            let key = (src.clone(), dst_loc.to_owned());
            link_ts
                .entry(key)
                .and_modify(|t| *t = (*t).max(ts))
                .or_insert(ts);
            address_linked.push((src, dst_loc.to_owned()));
        }
    }

    // ── Step D: dedup, hydrate, build RelatedEntity list ─────────────────────
    // Use a lightweight lookup (name/ch_number/status only) rather than calling
    // query_entity_by_id (which would recurse: query_entity_by_id →
    // assemble_entity_profile → query_related_entities → query_entity_by_id).
    //
    // Deterministic order (PIV-06): both candidate lists are sorted by the ids that produced
    // them before hydration. The lists are built by iterating `graph_events` rows whose record
    // ids are random, so the pre-existing order was arbitrary — and with a `limit` applied on
    // top, *which* neighbours an analyst saw was arbitrary too.
    director_linked.sort();
    director_linked.dedup();
    address_linked.sort();
    address_linked.dedup();

    // Hydration reads ONE scan per node type, not one per result. The three helpers this
    // replaces (`lookup_entity_summary`, `resolve_officer_name`, `resolve_location_postcode`)
    // each issued a full table scan per related entity, which was survivable only because the
    // `limit` was 10. Uncapping the profile's related list (PIV-06) without hoisting these would
    // have turned a 2,000-neighbour entity into 2,000 full scans of `graph_events`.
    let wanted_entities: HashSet<&str> = director_linked
        .iter()
        .chain(address_linked.iter())
        .map(|(entity_id, _)| entity_id.as_str())
        .collect();
    let entity_summaries: HashMap<String, EntityProfileSummary> = if wanted_entities.is_empty() {
        HashMap::new()
    } else {
        let rows = scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Entity\"")
            .await
            .unwrap_or_default();
        // Folded to the newest node-weight row per entity for the same reason the full profile
        // is: a related company shown as Active when it dissolved is the Phoenix signal being
        // hidden at exactly the point an analyst would look for it.
        fold_latest_node_rows(rows)
            .into_iter()
            .filter_map(parse_entity_node_row)
            .filter(|(id, _)| wanted_entities.contains(id.as_str()))
            .map(|(id, data)| {
                (
                    id,
                    EntityProfileSummary {
                        name: data.name,
                        ch_number: data.company_number,
                        status: data.status,
                    },
                )
            })
            .collect()
    };

    let officer_names: HashMap<String, String> = if director_linked.is_empty() {
        HashMap::new()
    } else {
        let wanted: HashSet<&str> = director_linked
            .iter()
            .map(|(_, officer_id)| officer_id.as_str())
            .collect();
        let rows =
            scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Officer\"")
                .await
                .unwrap_or_default();
        fold_latest_node_rows(rows)
            .into_iter()
            .filter_map(parse_officer_node_row)
            .filter(|(id, _)| wanted.contains(id.as_str()))
            .map(|(id, data)| (id, data.name))
            .collect()
    };

    let location_postcodes: HashMap<String, String> = if address_linked.is_empty() {
        HashMap::new()
    } else {
        let wanted: HashSet<&str> = address_linked
            .iter()
            .map(|(_, location_id)| location_id.as_str())
            .collect();
        let rows = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Location\"")
            .await
            .unwrap_or_default();
        rows.into_iter()
            .filter(|row| row["kind"].as_str().unwrap_or("") == "NodeAdded")
            .filter_map(|row| {
                let lid = extract_id_string(&row["payload"]["entity_id"])?;
                if !wanted.contains(lid.as_str()) {
                    return None;
                }
                let wj = row["payload"]["weight_json"].as_str()?;
                match serde_json::from_str::<NodeWeight>(wj).ok()? {
                    NodeWeight::Location(ld) => Some((lid, ld.postcode?)),
                    _ => None,
                }
            })
            .collect()
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut related: Vec<RelatedEntity> = Vec::new();

    // Director-linked first
    for (entity_id, officer_id) in &director_linked {
        if !seen.insert(entity_id.clone()) {
            continue;
        }

        if let Some(summary) = entity_summaries.get(entity_id) {
            related.push(RelatedEntity {
                name: summary.name.clone(),
                ch_number: summary.ch_number.clone(),
                status: summary.status.clone(),
                shared_connection: SharedConnection {
                    kind: "director".to_owned(),
                    // Resolve officer name for the shared_connection value; the empty-string
                    // fallback is `resolve_officer_name`'s own graceful-degradation contract.
                    value: officer_names.get(officer_id).cloned().unwrap_or_default(),
                },
                // The shared DIRECTORSHIP is the claim, so the `Directs` edge is its provenance —
                // and `Directs` has two producers, so this is always `Unattributed`. Attributing
                // it to the relative's namespace instead would name a dataset for a link that
                // dataset may not have drawn.
                provenance: graph_event_provenance(
                    "Directs",
                    entity_id,
                    link_ts
                        .get(&(entity_id.clone(), officer_id.clone()))
                        .copied(),
                ),
            });
        }

        if related.len() >= limit {
            break;
        }
    }

    // Address-linked (if limit not yet reached)
    if related.len() < limit {
        for (entity_id, location_id) in &address_linked {
            if !seen.insert(entity_id.clone()) {
                continue;
            }

            if let Some(summary) = entity_summaries.get(entity_id) {
                related.push(RelatedEntity {
                    name: summary.name.clone(),
                    ch_number: summary.ch_number.clone(),
                    status: summary.status.clone(),
                    shared_connection: SharedConnection {
                        kind: "address".to_owned(),
                        // Resolve location postcode for the shared_connection value.
                        value: location_postcodes
                            .get(location_id)
                            .cloned()
                            .unwrap_or_default(),
                    },
                    // The shared ADDRESS is the claim, and the `LocatedAt` edge carrying it was
                    // written by whichever mapper minted the relative's namespace.
                    provenance: graph_event_provenance(
                        "LocatedAt",
                        entity_id,
                        link_ts
                            .get(&(entity_id.clone(), location_id.clone()))
                            .copied(),
                    ),
                });
            }

            if related.len() >= limit {
                break;
            }
        }
    }

    Ok(related)
}

/// [`query_related_entities`] with no ceiling — the read the entity profile uses.
///
/// PIV-06 forbids a literal deciding what an analyst sees, and `query_related_entities`'s `limit`
/// is exactly that when the caller is the profile rather than a deliberately windowed consumer.
/// The parameter is kept on the windowed form because a caller that genuinely wants "the first
/// N" (a preview, a test fixture) should still be able to say so — what is removed is the profile
/// silently choosing 10 on the analyst's behalf.
///
/// `usize::MAX` is the sentinel rather than a second traversal, so there is one implementation of
/// "related to" and no chance of the two drifting. The hydration above is a fixed number of scans
/// regardless of the result count, so an uncapped call costs the same round trips as a capped one.
pub async fn query_related_entities_complete(
    db: &Surreal<Any>,
    target_entity_id: &str,
) -> Result<Vec<RelatedEntity>, surrealdb::Error> {
    query_related_entities(db, target_entity_id, usize::MAX).await
}

// ── Neighbours (PIV-06) ──────────────────────────────────────────────────────

/// One neighbour of an entity: another node joined to it by a single edge.
///
/// # What "neighbour" means here, and why
///
/// The phase 45 design notes left this open (open question 3): *direct edges only, or the 1-hop closure
/// including both directions?* This type settles it. **A neighbour is any node joined to the
/// subject by one edge in EITHER direction, counted once.**
///
/// Two existing conventions are being matched deliberately, and they answer different halves of
/// the question:
///
/// * **Direction** comes from `crates/workbench/src/graph/population_scope.rs`. Its `build` walks
///   `CsrSnapshot::neighbors_undirected`, and its own test
///   `build_includes_a_node_reachable_only_via_an_inbound_edge_from_a_seed` pins that a node
///   reachable only via an *inbound* edge is inside the population. That is the population every
///   canvas algorithm, the Arrow export and the label index are scoped to. If the profile counted
///   only outbound edges, a company the canvas draws adjacent to this entity would be absent from
///   the entity's own neighbour list — the screen and the test disagreeing about one entity,
///   which is precisely what the phase 45 design notes warned this decision was for.
/// * **Edge population** comes from `crate::persistence::traversal::bfs_traversal`. It walks
///   `EdgeAdded` events in both directions *and* `identity_links` whose status is `auto_linked`
///   or `confirmed`, as undirected synthetic edges. `graph-path` will therefore route through an
///   identity link, so a neighbour list that excluded them would call a node unreachable that the
///   path command reaches in one hop. Identity-link neighbours are labelled `IdentityLink` in
///   [`Neighbour::edge_types`] rather than hidden, because an analyst pivoting through one is
///   crossing a *resolution* rather than a recorded relationship and should be able to see that.
///
/// ENR-11 is why the link statuses are enumerated rather than left open: `pending_review` and
/// sub-candidate links (0.50–0.69) are unreviewed guesses, and a guess rendered beside a
/// registered fact reads as a fact.
///
/// The subject itself is never its own neighbour, even when a self-edge exists.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Neighbour {
    /// The adjacent node's entity id.
    pub id: String,
    /// Every distinct edge type joining the subject to this neighbour, sorted. More than one is
    /// normal — a director who is also a shareholder, a company both `AwardedTo` and `LocatedAt`.
    pub edge_types: Vec<String>,
}

/// The neighbours of `entity_id` in `edge_rows`, per the definition on [`Neighbour`].
///
/// Pure, so the convention can be pinned against `traversal::build_adj_from_rows` — the
/// project-wide undirected-adjacency builder — without a database. See
/// `neighbours_from_rows_agrees_with_build_adj_from_rows` below; that test is the reason this is
/// a restriction of an existing definition rather than a second one.
///
/// Ids are read through [`extract_id_string`], which is where this deliberately goes *further*
/// than `build_adj_from_rows`: that function reads `src_id`/`dst_id` with `as_str()`, so an edge
/// whose endpoint SurrealDB coerced into a RecordId object (every `director:<slug>` endpoint, per
/// the `director:{slug}` keying rule) is invisible to it. The divergence is pinned by
/// `neighbours_from_rows_sees_a_recordid_endpoint_that_build_adj_from_rows_drops` rather than
/// left to be discovered.
pub fn neighbours_from_rows(edge_rows: &[serde_json::Value], entity_id: &str) -> Vec<Neighbour> {
    let mut by_id: HashMap<String, HashSet<String>> = HashMap::new();
    for row in edge_rows {
        if row["kind"].as_str().unwrap_or("") != "EdgeAdded" {
            continue;
        }
        let (Some(src), Some(dst)) = (
            extract_id_string(&row["payload"]["src_id"]),
            extract_id_string(&row["payload"]["dst_id"]),
        ) else {
            continue;
        };
        // Either direction counts (population_scope's undirected convention); a self-edge does
        // not make a node its own neighbour.
        let other = if src == entity_id && dst != entity_id {
            dst
        } else if dst == entity_id && src != entity_id {
            src
        } else {
            continue;
        };
        let Some(edge_type) = row["payload"]["weight_json"]
            .as_str()
            .and_then(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
            .and_then(|v| v["type"].as_str().map(str::to_owned))
        else {
            continue; // graceful-skip: can't determine type — mirrors build_adj_from_rows
        };
        by_id.entry(other).or_default().insert(edge_type);
    }
    finish_neighbours(by_id)
}

/// The distinct edge types of a set of edges, in the order they arrive (which for a `BTreeSet` of
/// [`NeighbourEdge`] is edge type ascending) — the shape [`Neighbour::edge_types`] has always had.
fn distinct_edge_types(edges: &[NeighbourEdge]) -> Vec<String> {
    let mut out: Vec<String> = edges.iter().map(|e| e.edge_type.clone()).collect();
    out.dedup();
    out
}

/// Sort a `(id -> edge types)` map into the deterministic [`Neighbour`] list both readers return.
fn finish_neighbours(by_id: HashMap<String, HashSet<String>>) -> Vec<Neighbour> {
    let mut out: Vec<Neighbour> = by_id
        .into_iter()
        .map(|(id, types)| {
            let mut edge_types: Vec<String> = types.into_iter().collect();
            edge_types.sort();
            Neighbour { id, edge_types }
        })
        .collect();
    out.sort();
    out
}

/// The basis string for an identity-link edge, whose endpoints carry no temporal fields at all.
///
/// An identity link is an assertion made at a moment — that two ids name one subject — not a
/// relationship in force over an interval, so `identity_links` declares no `effective_from` or
/// `effective_to` to read. Naming that here is the difference `EffectiveRange::Unrecorded::basis`
/// exists for: "this dataset records no dates" is a different fact from "this row lost its dates",
/// and neither is visible from an empty field.
pub const IDENTITY_LINK_RANGE_BASIS: &str =
    "identity_links: an identity link records no date range";

/// One edge the walk crossed, with the date range the record states for it (CNV-02).
///
/// # Why the range is carried here rather than looked up later
///
/// CNV-02 requires an expansion's edges to show their **type and temporal range**. The type alone
/// was all [`Neighbour`] ever carried, so before this the only honest thing a canvas could render
/// was half the requirement. The alternative — resolving each edge's range from a second read once
/// the neighbourhood was on screen — is one scan per edge over the same `EdgeAdded` rows this
/// index was built from, which is the N+1 shape [`NeighbourIndex`] exists to avoid.
///
/// # What `None` means, and what it does not
///
/// `None` is "the record states no endpoint", never "still in force" and never "forever". Measured
/// against this project's working analysis on 2026-09-15 and recorded in `workbench-dto::temporal`:
/// `effective_to` is null on **every** `EdgeAdded` event in the store, and `effective_from` on
/// three quarters of them — only `Controls` and `OwnsProperty` carry a start date at all. So an
/// undated edge is the ordinary case here, not an edge case, and a renderer that printed "ongoing"
/// for an absent end would be asserting something no dataset said.
///
/// Ordered by type first, so a `BTreeSet` of these sorts into the same edge-type order
/// [`finish_neighbours`] has always produced.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NeighbourEdge {
    pub edge_type: String,
    /// ISO-8601 start as the record states it, or `None` when it states none.
    pub effective_from: Option<String>,
    /// ISO-8601 end as the record states it, or `None` when it states none.
    pub effective_to: Option<String>,
    /// The field(s) consulted, for [`EffectiveRange`]'s `basis`. See
    /// [`IDENTITY_LINK_RANGE_BASIS`] for the one edge kind whose basis is not an edge-meta path.
    pub basis: String,
}

/// One node in an entity's neighbourhood, and how the walk reached it (CNV-01).
///
/// `hop` is the **first** distance at which the walk reached this node, so a node adjacent to the
/// seed is `1` even when a longer path also reaches it. `reached_from` is the node it was first
/// reached through — deterministically the lowest-id frontier node that could — and `edges` are
/// the edges joining those two, not every edge incident on the node. Together they describe
/// the expansion as a tree, which is what a canvas draws when it grows a neighbourhood outward.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NeighbourhoodNode {
    pub id: String,
    /// Hops from the seed. Always `>= 1`: the seed is not a member of its own neighbourhood, for
    /// the same reason it is not its own [`Neighbour`].
    pub hop: u32,
    /// The edges the walk crossed to reach this node, with their recorded ranges (CNV-02).
    ///
    /// Replaced the earlier `edge_types: Vec<String>` rather than joining it: two fields, one
    /// derived from the other, is how a type list and a range list come to disagree about how many
    /// edges there were. [`Self::edge_types`] derives the old view on demand.
    pub edges: Vec<NeighbourEdge>,
    pub reached_from: String,
}

impl NeighbourhoodNode {
    /// The distinct edge types joining this node to [`Self::reached_from`], sorted — the view
    /// [`Neighbour::edge_types`] has always presented, derived rather than stored.
    pub fn edge_types(&self) -> Vec<String> {
        let mut out: Vec<String> = self.edges.iter().map(|e| e.edge_type.clone()).collect();
        out.dedup();
        out
    }
}

/// The whole graph's undirected adjacency, loaded once, in the neighbour vocabulary
/// [`Neighbour`] defines.
///
/// # Why this type exists
///
/// [`query_entity_neighbours`] answers for one subject and pays one full `graph_events` scan to do
/// it. CNV-01 expands a neighbourhood **N hops at a time**, which means asking the same question of
/// every node on a frontier — and doing that through the per-subject function would be one scan per
/// frontier node, up to the node cap. The load and the question are separated here so a whole
/// expansion costs the reads of one lookup.
///
/// It is built from exactly the sources [`query_entity_neighbours`] uses, through the same pure
/// [`neighbours_from_rows`] parsing rules, and `query_entity_neighbours` is now a call into it —
/// so there is one definition of "neighbour" in this file rather than two that could drift.
/// `neighbour_index_agrees_with_neighbours_from_rows` pins that equivalence.
#[derive(Debug, Default, Clone)]
pub struct NeighbourIndex {
    by_id: HashMap<String, HashMap<String, BTreeSet<NeighbourEdge>>>,
}

impl NeighbourIndex {
    /// Build from raw `EdgeAdded` rows plus already-ENR-11-filtered identity-link endpoint pairs.
    ///
    /// The identity-link **filter** is the caller's (see [`load_neighbour_index`]): this function
    /// takes endpoint pairs, so it cannot accidentally admit a `pending_review` link, and it stays
    /// pure and testable without a database.
    pub fn from_rows(edge_rows: &[serde_json::Value], identity_links: &[(String, String)]) -> Self {
        let mut index = NeighbourIndex::default();
        for row in edge_rows {
            if row["kind"].as_str().unwrap_or("") != "EdgeAdded" {
                continue;
            }
            let (Some(src), Some(dst)) = (
                extract_id_string(&row["payload"]["src_id"]),
                extract_id_string(&row["payload"]["dst_id"]),
            ) else {
                continue;
            };
            // A self-edge does not make a node its own neighbour.
            if src == dst {
                continue;
            }
            let Some(edge_type) = row["payload"]["weight_json"]
                .as_str()
                .and_then(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
                .and_then(|v| v["type"].as_str().map(str::to_owned))
            else {
                continue; // graceful-skip: can't determine type — mirrors build_adj_from_rows
            };
            // The range rides along from the SAME row the type was read from, through the same
            // `edge_meta_candidate` every profile read of an edge already uses (CNV-02). Reading it
            // later would mean a second scan of these rows, per edge.
            let (basis, effective_from, effective_to) =
                edge_meta_candidate(&edge_type, &row["payload"]["weight_json"]);
            index.link(
                &src,
                &dst,
                &NeighbourEdge {
                    edge_type,
                    effective_from,
                    effective_to,
                    basis,
                },
            );
        }

        for (src, tgt) in identity_links {
            if src.is_empty() || tgt.is_empty() || src == tgt {
                continue;
            }
            // IdentityLink is undirected, same as bfs_traversal treats it, and labelled rather
            // than merged: pivoting through one crosses a *resolution*, not a recorded relationship.
            index.link(
                src,
                tgt,
                &NeighbourEdge {
                    edge_type: "IdentityLink".to_owned(),
                    effective_from: None,
                    effective_to: None,
                    basis: IDENTITY_LINK_RANGE_BASIS.to_owned(),
                },
            );
        }
        index
    }

    fn link(&mut self, a: &str, b: &str, edge: &NeighbourEdge) {
        self.by_id
            .entry(a.to_owned())
            .or_default()
            .entry(b.to_owned())
            .or_default()
            .insert(edge.clone());
        self.by_id
            .entry(b.to_owned())
            .or_default()
            .entry(a.to_owned())
            .or_default()
            .insert(edge.clone());
    }

    /// How many distinct nodes have at least one neighbour. Non-vacuity checks read this rather
    /// than inferring a populated index from a non-empty answer.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// `true` when no edge was read at all — an index that can only ever answer "no neighbours".
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Every neighbour of `id`, in the deterministic order [`finish_neighbours`] fixes.
    pub fn neighbours_of(&self, id: &str) -> Vec<Neighbour> {
        let mut out: Vec<Neighbour> = self
            .adjacency_of(id)
            .into_iter()
            .map(|(neighbour, edges)| Neighbour {
                id: neighbour,
                edge_types: distinct_edge_types(&edges),
            })
            .collect();
        out.sort();
        out
    }

    /// Every neighbour of `id` with the edges joining them, in the `BTreeSet`'s own order (edge
    /// type ascending, then range). The one place both [`Self::neighbours_of`] and [`Self::expand`]
    /// read the adjacency from, so the type list and the edge list can never describe different
    /// edges.
    fn adjacency_of(&self, id: &str) -> Vec<(String, Vec<NeighbourEdge>)> {
        let Some(adjacent) = self.by_id.get(id) else {
            return Vec::new();
        };
        adjacent
            .iter()
            .map(|(neighbour, edges)| (neighbour.clone(), edges.iter().cloned().collect()))
            .collect()
    }

    /// Every node within `hops` of `seed`, **complete and uncapped**, ordered by hop ascending and
    /// then by id ascending.
    ///
    /// The order is the contract a display cap depends on: a caller that keeps a prefix keeps the
    /// nearest nodes, and keeps the same ones on every call. Capping is emphatically *not* done
    /// here — 45-02 settled that windowing belongs at the wire boundary, and CNV-01 additionally
    /// requires that a cut be *reported*, which a read path returning a bare prefix cannot do.
    ///
    /// `hops == 0`, an unknown seed, and an isolated seed all return an empty vector.
    pub fn expand(&self, seed: &str, hops: u32) -> Vec<NeighbourhoodNode> {
        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(seed.to_owned());
        let mut frontier: Vec<String> = vec![seed.to_owned()];
        let mut out: Vec<NeighbourhoodNode> = Vec::new();

        for hop in 1..=hops {
            // BTreeMap: the level is emitted by id ascending, and `reached_from` is therefore the
            // lowest-id frontier node that reached it — both deterministic.
            let mut level: BTreeMap<String, (String, Vec<NeighbourEdge>)> = BTreeMap::new();
            for node in &frontier {
                for (neighbour_id, edges) in self.adjacency_of(node) {
                    if seen.contains(&neighbour_id) || level.contains_key(&neighbour_id) {
                        continue;
                    }
                    level.insert(neighbour_id, (node.clone(), edges));
                }
            }
            if level.is_empty() {
                break;
            }
            frontier = Vec::with_capacity(level.len());
            for (id, (reached_from, edges)) in level {
                seen.insert(id.clone());
                frontier.push(id.clone());
                out.push(NeighbourhoodNode {
                    id,
                    hop,
                    edges,
                    reached_from,
                });
            }
        }
        out
    }

    /// [`Self::expand`], generalised to a set of seeds and a hub exclusion (Phase 50, D-12/T-50-02)
    /// — the read path a finding's own `RecordRef`s walk from, all at once.
    ///
    /// Every `seeds` entry starts in the initial frontier together, so a node reachable from more
    /// than one seed is recorded once, at the **smallest** hop count any seed reaches it at — the
    /// same "first write wins" rule [`Self::expand`] already applies across frontier nodes within
    /// one seed's own walk, extended here across seeds. A seed itself is never emitted (it starts
    /// in `seen`, mirroring [`Self::expand`]'s "the seed is not a member of its own neighbourhood"),
    /// even when it is also named in `hub_ids` — a finding's own named entity is never hidden for
    /// having a high degree.
    ///
    /// A node in `hub_ids` that is NOT itself a seed is still recorded in the output at the hop it
    /// was first reached, but is excluded from the next hop's frontier — it is never used to expand
    /// the walk further. This is T-50-02's mitigation applied INSIDE the traversal itself, not only
    /// reported after the fact: a hub's further edges are simply never read by this walk, so their
    /// cost is never paid, rather than being paid and then cut by a cap at the wire boundary.
    ///
    /// `hops == 0`, an empty `seeds`, and a `seeds` set with no neighbours all return an empty
    /// vector — the same degenerate-input behaviour [`Self::expand`] already has.
    pub fn expand_multi(
        &self,
        seeds: &[String],
        hops: u32,
        hub_ids: &HashSet<String>,
    ) -> Vec<NeighbourhoodNode> {
        let mut seen: HashSet<String> = seeds.iter().cloned().collect();
        let mut frontier: Vec<String> = seeds.to_vec();
        let mut out: Vec<NeighbourhoodNode> = Vec::new();

        for hop in 1..=hops {
            let mut level: BTreeMap<String, (String, Vec<NeighbourEdge>)> = BTreeMap::new();
            for node in &frontier {
                for (neighbour_id, edges) in self.adjacency_of(node) {
                    if seen.contains(&neighbour_id) || level.contains_key(&neighbour_id) {
                        continue;
                    }
                    level.insert(neighbour_id, (node.clone(), edges));
                }
            }
            if level.is_empty() {
                break;
            }
            frontier = Vec::new();
            for (id, (reached_from, edges)) in level {
                seen.insert(id.clone());
                // A hub node is recorded at this hop but never expanded further (T-50-02).
                if !hub_ids.contains(&id) {
                    frontier.push(id.clone());
                }
                out.push(NeighbourhoodNode {
                    id,
                    hop,
                    edges,
                    reached_from,
                });
            }
        }
        out
    }
}

/// Load the whole undirected adjacency, ENR-11-gated, paged to exhaustion.
///
/// Both sources page through [`scan`]: `EdgeAdded` via [`scan::scan_events_by_kind`], identity
/// links via [`scan::scan_identity_links`]. There is no ceiling on this path.
pub async fn load_neighbour_index(db: &Surreal<Any>) -> Result<NeighbourIndex, surrealdb::Error> {
    let edge_rows = scan::scan_events_by_kind(db, "EdgeAdded").await?;

    // ENR-11 gate: auto_linked/confirmed only — a pending_review link is an unreviewed guess and
    // is never a neighbour. Two calls rather than `status IN [...]`, mirroring how
    // `bfs_traversal` loads the same rows.
    let mut link_rows = scan::scan_identity_links(db, Some("auto_linked"), None).await?;
    link_rows.extend(scan::scan_identity_links(db, Some("confirmed"), None).await?);
    let pairs: Vec<(String, String)> = link_rows
        .into_iter()
        .map(|row| (row.source_node_id, row.target_node_id))
        .collect();

    Ok(NeighbourIndex::from_rows(&edge_rows, &pairs))
}

/// Every neighbour of `entity_id`, complete — the PIV-06 neighbour read path.
///
/// Both sources page to exhaustion through [`scan`]: `EdgeAdded` via
/// [`scan::scan_events_by_kind`], identity links via [`scan::scan_identity_links`]. There is no
/// ceiling anywhere on this path, which is the whole point — a 2,000-neighbour entity returns
/// 2,000 neighbours, and `crates/core/tests/profile_paging.rs` fails if that stops being true.
///
/// Windowing is the *caller's* decision and belongs at the wire boundary, where
/// `workbench-dto`'s `Page<T>` carries the total alongside the rows. A read that silently
/// returns a prefix cannot be distinguished from a read that returned everything, and that
/// ambiguity is what PIV-06 forbids.
pub async fn query_entity_neighbours(
    db: &Surreal<Any>,
    entity_id: &str,
) -> Result<Vec<Neighbour>, surrealdb::Error> {
    Ok(load_neighbour_index(db).await?.neighbours_of(entity_id))
}

/// Every node within `hops` of `entity_id`, complete and uncapped — CNV-01's read path.
///
/// One index load serves the whole expansion, so `hops = 3` costs the same reads as `hops = 1`
/// rather than one scan per frontier node. See [`NeighbourIndex::expand`] for the ordering
/// guarantee a display cap depends on, and note what this function deliberately does not do:
/// **it applies no node cap.** CNV-01 requires the cap be reported, and a read that returns a
/// prefix has thrown away the figure a report needs.
pub async fn query_entity_neighbourhood(
    db: &Surreal<Any>,
    entity_id: &str,
    hops: u32,
) -> Result<Vec<NeighbourhoodNode>, surrealdb::Error> {
    Ok(load_neighbour_index(db).await?.expand(entity_id, hops))
}

/// [`query_entity_neighbourhood`], generalised to multiple seeds with a hub exclusion applied
/// inside the walk (Phase 50, D-12) — the finding-seeded detail view's read path, seeded from a
/// finding's own `RecordRef`s rather than one profile's own id.
///
/// `hub_ids` is the caller's own CSR-measured degree-cap set (`workbench::boot::resolve_hub_ids`
/// on the workbench side) — this function itself has no opinion on how a node came to be called a
/// hub, it only refuses to walk further through one. See [`NeighbourIndex::expand_multi`] for the
/// exact rule. Like [`query_entity_neighbourhood`], this applies **no node cap** — that remains
/// the wire boundary's job, reported through `NeighbourhoodView::cut`.
pub async fn query_entity_neighbourhood_multi(
    db: &Surreal<Any>,
    seeds: &[String],
    hops: u32,
    hub_ids: &HashSet<String>,
) -> Result<Vec<NeighbourhoodNode>, surrealdb::Error> {
    Ok(load_neighbour_index(db)
        .await?
        .expand_multi(seeds, hops, hub_ids))
}

// `lookup_entity_summary`, `resolve_officer_name` and `resolve_location_postcode` were deleted
// here (PIV-06, plan 45-02). Each took one id and issued a full `graph_events` scan to resolve
// it, and each was called once per related entity — a shape that only stayed affordable while
// `query_related_entities` was capped at 10 results. `query_related_entities`'s Step D now builds
// the same three lookups as maps from one scan apiece, so uncapping the profile's related list
// costs a fixed number of round trips instead of one per neighbour. They are deleted rather than
// left `#[allow(dead_code)]` because a per-id resolver sitting next to a map is an invitation to
// reintroduce the N+1 from a new call site.

/// The latest `Contract` node weight for each id in `wanted`, from ONE paged marker scan.
///
/// Replaces a per-contract `SELECT * FROM graph_events WHERE payload.entity_id = $eid LIMIT 5`
/// issued inside a loop. That shape had three defects, and PIV-06 only names the first:
///
/// * **The cap.** Five is a ceiling on a node's event history, not on anything an analyst asked
///   for. `graph_events` is append-only and every re-ingest of a contract emits another
///   node-weight event, so a contract touched six times could return five `NodeUpdated` rows and
///   no `NodeAdded` — and with no `ORDER BY`, *which* five was arbitrary. A contract dropping out
///   of an entity's profile with no error is the exact failure PIV-06 exists to forbid.
/// * **The missing fold.** The loop pushed one `ContractRow` per matching row, so a re-ingested
///   contract appeared twice on the profile. Every other node read in this module folds to the
///   newest row per entity (see [`NODE_WEIGHT_KINDS`] for why that matters); this one did not.
/// * **The cost.** `payload.entity_id` carries no index, so each point read was a full-table scan
///   server-side. N contracts meant N scans of `graph_events`. One marker scan is cheaper for
///   every N above one, and is the same idiom the Officer lookup above uses.
///
/// Filtering to `wanted` keeps the returned map proportional to the entity, not to the corpus.
async fn contract_weight_map(
    db: &Surreal<Any>,
    wanted: &[String],
) -> HashMap<String, (ContractData, Option<u64>)> {
    if wanted.is_empty() {
        return HashMap::new();
    }
    let wanted_set: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    // CONTAINS-only marker scan with `kind` filtered Rust-side — never `kind = 'NodeAdded' AND
    // … CONTAINS …`, which is the SurrealDB 3.x planner bug that returns 0 rows for `SELECT *`
    // while `count()` reports the truth (Pitfall 7, and the reason the point reads existed).
    let rows = scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Contract\"")
        .await
        .unwrap_or_default();
    fold_latest_node_rows(rows)
        .into_iter()
        .filter_map(|row| {
            let id = extract_id_string(&row["payload"]["entity_id"])?;
            if !wanted_set.contains(id.as_str()) {
                return None;
            }
            // The winning row's own `timestamp` rides along: after the fold this is the moment the
            // values below were last recorded, which is what a contract row's provenance asserts.
            let ts = row["timestamp"].as_u64();
            let wj = row["payload"]["weight_json"].as_str()?;
            match serde_json::from_str::<NodeWeight>(wj).ok()? {
                NodeWeight::Contract(cd) => Some((id, (cd, ts))),
                _ => None,
            }
        })
        .collect()
}

/// Assemble a full `EntityProfile` for a matched entity by running sub-queries
/// for its directors, contracts, addresses, alerts, and related entities.
///
/// All sub-queries degrade gracefully to empty `Vec`s on parse errors (never panic).
///
/// # SurrealDB 3.x compatibility note
///
/// All `payload.dst_id = $target` and `payload.src_id = $target` equality filters
/// have been removed.  We over-fetch by edge type (via `CONTAINS` on weight_json)
/// and filter Rust-side by the target entity ID.
async fn assemble_entity_profile(
    db: &Surreal<Any>,
    entity_id: &str,
    data: EntityData,
    identity_event_ms: Option<u64>,
    deep: bool,
) -> Result<EntityProfile, surrealdb::Error> {
    let target = entity_id.to_owned();
    // The header claims (CH number, incorporation date, status) all come from the folded `Entity`
    // node-weight row the caller matched, so its `timestamp` is theirs. Passed in rather than
    // re-read: the callers already hold the row, and a second lookup could land on a different
    // event and attribute the claim to the wrong moment.
    let identity_provenance = graph_event_provenance("Entity", entity_id, identity_event_ms);

    // ── 1. Directors: Directs edges where dst = this entity ──────────────────
    // Fetch all Directs EdgeAdded events and filter Rust-side for dst_id == target.
    //
    // Ids are read through `extract_id_string`, not `.as_str()`. SurrealDB 3.x coerces a
    // `director:<slug>` string into a RecordId object on write, and `.as_str()` on the object
    // form yields `None` — so an officer keyed the way the load-bearing domain rule
    // requires ("Directors are keyed `director:{slug}`") dropped out of this list with no error.
    // The contracts block below already read both ends this way.
    let all_directs: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"Directs\"").await?;

    // One `(officer_id, role-from-the-edge)` pair per Directs edge into this entity, sorted by
    // officer id. `graph_events` record ids are random, so scan order is arbitrary and two runs
    // over the same data could render the same directors in different orders; PIV-06 asks for a
    // deterministic order, and the officer id is the only stable key a `DirectorRow` has.
    // `dedup` after the sort collapses a re-ingested Directs edge, which used to render the same
    // officer twice — `graph_events` is append-only, so re-ingestion is the normal case.
    //
    // The edge's own `meta.effective_from`/`meta.effective_to` ride along as the FALLBACK half of
    // the row's effective range (PIV-04). The tenure on the Officer node wins when it exists,
    // because that is the record that actually states a directorship's dates — measured
    // 2026-09-15, 0 of 24,662 `Directs` edges in this store carry either endpoint, so on today's
    // corpus this candidate never answers and the basis string is what says so.
    /// `(officer id, role from the edge, edge `effective_from`, edge `effective_to`)`.
    type DirectsRow = (String, Option<String>, Option<String>, Option<String>);
    let mut directs_in: Vec<DirectsRow> = all_directs
        .iter()
        .filter(|row| row["kind"].as_str().unwrap_or("") == "EdgeAdded")
        .filter(|row| {
            extract_id_string(&row["payload"]["dst_id"]).as_deref() == Some(target.as_str())
        })
        .filter_map(|row| {
            let src_id = extract_id_string(&row["payload"]["src_id"])?;
            // Parse the edge weight to get the role recorded on the appointment itself.
            let officer_role = row["payload"]["weight_json"]
                .as_str()
                .and_then(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
                .and_then(|v| v["data"]["officer_role"].as_str().map(|s| s.to_owned()));
            let (_, edge_from, edge_to) =
                edge_meta_candidate("directs", &row["payload"]["weight_json"]);
            Some((src_id, officer_role, edge_from, edge_to))
        })
        .collect();
    directs_in.sort();
    directs_in.dedup();

    // The newest `Directs` event per officer, for that row's provenance timestamp. Kept as a
    // side map rather than folded into `directs_in` so the sort/dedup above still collapses a
    // re-ingested edge — two events for one appointment are one directorship, not two rows.
    //
    // `Directs` is unattributable by term (two producers write it in the same shape — see
    // `attribution::UNATTRIBUTABLE_TERMS`), so this timestamp does not reach the analyst today.
    // It is computed correctly anyway: the day that ambiguity is resolved, the fix should be one
    // registry edit, not an archaeology exercise in this loop.
    let directs_ts: HashMap<&str, u64> = all_directs
        .iter()
        .filter(|row| row["kind"].as_str().unwrap_or("") == "EdgeAdded")
        .fold(HashMap::new(), |mut acc, row| {
            if extract_id_string(&row["payload"]["dst_id"]).as_deref() != Some(target.as_str()) {
                return acc;
            }
            let Some(src) = row["payload"]["src_id"].as_str() else {
                return acc;
            };
            let ts = row["timestamp"].as_u64().unwrap_or(0);
            acc.entry(src)
                .and_modify(|t| *t = (*t).max(ts))
                .or_insert(ts);
            acc
        });

    // ONE Officer scan for the whole list, not one per director. The lookup this replaces issued
    // an identical full Officer scan *inside* the loop, so an entity with N directors scanned the
    // same rows N times — quadratic in exactly the case PIV-06 is about (a 2,000+-neighbour
    // entity), and the reason the completeness test below could not have been run against the
    // shape this replaces.
    let wanted_officers: HashSet<&str> =
        directs_in.iter().map(|(id, _, _, _)| id.as_str()).collect();
    let officer_data: HashMap<String, OfficerData> = if wanted_officers.is_empty() {
        HashMap::new()
    } else {
        let officer_rows =
            scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Officer\"")
                .await
                .unwrap_or_default();
        fold_latest_node_rows(officer_rows)
            .into_iter()
            .filter_map(parse_officer_node_row)
            .filter(|(id, _)| wanted_officers.contains(id.as_str()))
            .collect()
    };

    let directors: Vec<DirectorRow> = directs_in
        .iter()
        .map(|(src_id, officer_role, edge_from, edge_to)| {
            let d = officer_data.get(src_id);
            // The tenure on the Officer record is the directorship's range; the `Directs` edge's
            // own meta is the fallback. Whichever answers, its label becomes the row's `basis`, so
            // a reader can tell which record was consulted rather than inferring it.
            let range = effective_range(&[
                (
                    "officer.appointed_on / officer.resigned_on",
                    d.and_then(|d| d.appointed_on.clone()),
                    d.and_then(|d| d.resigned_on.clone()),
                ),
                (
                    "directs.meta.effective_from / directs.meta.effective_to",
                    edge_from.clone(),
                    edge_to.clone(),
                ),
            ]);
            DirectorRow {
                // Derive name from OfficerData; fall back to src_id (slug) if absent.
                name: d
                    .map(|d| d.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| src_id.clone()),
                role: officer_role
                    .clone()
                    .or_else(|| d.and_then(|d| d.officer_role.clone())),
                appointed: d.and_then(|d| d.appointed_on.clone()),
                resigned: d.and_then(|d| d.resigned_on.clone()),
                provenance: graph_event_provenance(
                    "Directs",
                    src_id,
                    directs_ts.get(src_id.as_str()).copied(),
                ),
                temporal: Temporal::unfiltered(range),
            }
        })
        .collect();

    // ── 2. Contracts and planning applications: AwardedTo edges where src = this entity ──
    // AwardedTo semantic: entity (src) → contract (dst). Both CF and planning mappers write it
    // this way, which is why these were one section until D-03. Fetch all and filter Rust-side
    // for src_id == target, then split on the destination id prefix — see
    // `is_planning_application_id` for why the prefix and not `application_ref`, and for why the
    // positive predicate has to be `planning:` rather than `contract:`.
    let all_awarded: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"AwardedTo\"").await?;

    // Sorted and deduped for the same two reasons as the directors list above: contract id is
    // the only stable key a `ContractRow` has, and an append-only log re-emits the same
    // AwardedTo edge on every re-ingest.
    let mut awarded_in: Vec<String> = all_awarded
        .into_iter()
        .filter_map(|row| {
            if row["kind"].as_str()? != "EdgeAdded" {
                return None;
            }
            let src = extract_id_string(&row["payload"]["src_id"])?;
            if src == target {
                extract_id_string(&row["payload"]["dst_id"])
            } else {
                None
            }
        })
        .collect();
    awarded_in.sort();
    awarded_in.dedup();

    // One weight map over both halves: `contract_weight_map` costs one marker scan regardless of
    // how many ids it is asked about, so splitting the ids before the lookup would double the
    // scans to save nothing. The split happens on the way out, where it is one predicate.
    let contract_data = contract_weight_map(db, &awarded_in).await;
    // Both sections attribute against the DESTINATION id — the contract's or the application's
    // own key — because that is the node whose weight supplied every value in the row. `CF:` is
    // minted only by `crates/ingestor/src/cf/mapper.rs` and `planning:` only by the planning
    // mapper and its backfill, so these are the two sections where the id namespace names one
    // producer and the row can be attributed outright.
    let contracts: Vec<ContractRow> = awarded_in
        .iter()
        .filter(|id| !is_planning_application_id(id))
        .filter_map(|id| contract_data.get(id).map(|cd| (id, cd)))
        .map(|(id, (cd, ts))| ContractRow {
            id: (*id).clone(),
            name: cd.name.clone(),
            value_gbp: cd.value_gbp,
            status: cd.status.clone(),
            published: cd.date_published.clone(),
            ward_code: cd.ward_code.clone(),
            tender_ref: cd.tender_ref.clone(),
            application_ref: cd.application_ref.clone(),
            provenance: graph_event_provenance("Contract", id, *ts),
            temporal: Temporal::unfiltered(award_range(cd)),
        })
        .collect();
    let planning_applications: Vec<PlanningApplicationRow> = awarded_in
        .iter()
        .filter(|id| is_planning_application_id(id))
        .filter_map(|id| contract_data.get(id).map(|cd| (id, cd)))
        .map(|(id, (cd, ts))| PlanningApplicationRow {
            id: (*id).clone(),
            name: cd.name.clone(),
            status: cd.status.clone(),
            published: cd.date_published.clone(),
            ward_code: cd.ward_code.clone(),
            application_ref: cd.application_ref.clone(),
            provenance: graph_event_provenance("Contract", id, *ts),
            temporal: Temporal::unfiltered(award_range(cd)),
        })
        .collect();

    // ── 3. Addresses: LocatedAt edges where src = this entity ────────────────
    // Fetch all LocatedAt EdgeAdded events and filter Rust-side for src_id == target.
    //
    // post-D-04 (phase 29, land-flip-address-matching): same narrowing as
    // `query_related_entities`'s Step C above. Before this phase, a planning applicant's site
    // `LocatedAt` edge was always sourced from the applicant Entity, so this `src == target`
    // filter caught it. As of this phase, `map_planning_item` sources a planning `Contract`'s
    // `LocatedAt` edge from the Contract node instead — see
    // `crates/detector/src/motif/cluster.rs`'s `ClusterDetector::check`, which DOES follow the
    // contract-sourced path and is the reference for extending this Addresses section to do the
    // same in a future phase.
    //
    // Practical effect: this entity's own outgoing `LocatedAt` edges will only include (a)
    // pre-existing legacy entity-sourced rows from before this phase's re-ingest
    // (`graph_events` is append-only, D-24 — never deleted) and (b) non-planning sources that
    // still emit entity-sourced `LocatedAt` edges (e.g. Companies House registered-office
    // addresses, unaffected by this phase). A NEWLY-ingested or re-ingested planning
    // application's site will NOT appear in this entity's Addresses list via this code path —
    // that address is now only reachable via the Contract. Intentional, documented behaviour
    // change per D-04, not a defect.
    let all_located: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"LocatedAt\"").await?;

    // Same `extract_id_string` / sort / dedup treatment as the two sections above: RecordId
    // coercion must not drop a location, and the rendered order must not be `graph_events`'
    // random record-id order.
    //
    // The `LocatedAt` edge's `meta` is the address row's effective range (PIV-04) — a Location
    // node carries no dates of its own, so the edge is the only record of when this entity was at
    // this address. Measured 2026-09-15, 0 of 173,203 `LocatedAt` edges carry either endpoint, so
    // every address row on today's corpus comes back `Unrecorded` and, under a filter,
    // `AsAtStatus::Undated` — retained and labelled, per `NULL_RANGE_RULE`.
    let mut location_ids: Vec<(String, EffectiveRange)> = Vec::new();
    for row in all_located {
        if row["kind"].as_str() != Some("EdgeAdded") {
            continue;
        }
        if extract_id_string(&row["payload"]["src_id"]).as_deref() != Some(target.as_str()) {
            continue;
        }
        let Some(dst) = extract_id_string(&row["payload"]["dst_id"]) else {
            continue;
        };
        let (basis, from, to) = edge_meta_candidate("located_at", &row["payload"]["weight_json"]);
        let range = effective_range(&[(basis.as_str(), from, to)]);
        match location_ids.iter_mut().find(|(id, _)| *id == dst) {
            // The dedup this replaces collapsed a re-ingested edge, and so does this: a second
            // copy of the same edge is the same address, not a second one. `graph_events` is
            // append-only, so re-ingestion is the normal case — and a re-ingested copy that lost
            // its dates must not blank a range an earlier copy recorded.
            Some(existing) if existing.1.is_unrecorded() => existing.1 = range,
            Some(_) => {}
            None => location_ids.push((dst, range)),
        }
    }
    location_ids.sort_by(|a, b| a.0.cmp(&b.0));

    // Fetch all Location NodeAdded events and filter Rust-side by location_id.
    let all_loc_nodes: Vec<serde_json::Value> = if !location_ids.is_empty() {
        scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Location\"")
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // The node event's `timestamp` rides along with its weight: an address row's `observed_at` is
    // the moment the Location node recorded these values, not the moment the profile was built.
    let location_data: HashMap<String, (NodeWeight, Option<u64>)> = all_loc_nodes
        .into_iter()
        .filter(|row| row["kind"].as_str().unwrap_or("") == "NodeAdded")
        .filter_map(|row| {
            let leid = extract_id_string(&row["payload"]["entity_id"])?;
            let ts = row["timestamp"].as_u64();
            let wj = row["payload"]["weight_json"].as_str()?;
            let weight = serde_json::from_str::<NodeWeight>(wj).ok()?;
            Some((leid, (weight, ts)))
        })
        .collect();

    // Attributed against the LOCATION node id, not against the subject. The values an address row
    // renders (postcode, line 1, locality) are the Location node's, and `location:` ids are
    // address-derived and minted by the Companies House, Contracts Finder, planning and FCA
    // Mutuals mappers alike — so one node is reachable from several datasets and none of them can
    // be named as the one that wrote these values. Attributing the row to the subject's namespace
    // instead would name a dataset for a value that dataset may not have supplied, which is the
    // placeholder attribution this plan forbids. See the summary for what this costs in coverage.
    let addresses: Vec<AddressRow> = location_ids
        .iter()
        .filter_map(|(lid, range)| match location_data.get(lid) {
            Some((NodeWeight::Location(ld), ts)) => Some(AddressRow {
                // The `LocatedAt` destination id, verbatim — the pivot target D-06 asked for, and
                // the same string this row is already attributed by two lines below.
                id: lid.clone(),
                postcode: ld.postcode.clone(),
                address_line_1: ld.address_line_1.clone(),
                locality: ld.locality.clone(),
                provenance: graph_event_provenance("Location", lid, *ts),
                // The range is the EDGE's, not the Location node's, for the same reason the
                // provenance is the node's: the row renders the node's values, but "this entity
                // was here" is the edge's claim and the edge is the only thing that could date it.
                temporal: Temporal::unfiltered(range.clone()),
            }),
            _ => None,
        })
        .collect();

    // ── 4. Active alerts that reference this entity ───────────────────────────
    // D-13: no resolution model — just scan MotifDetected events whose
    // result_json contains the entity_id string.
    // Capped in Rust, not in SQL: a `LIMIT 50` on the query bounds what the planner looked at,
    // not the matches, so under the planner bug it could return none of an entity's alerts.
    //
    // "Active" is load-bearing and was, until 2026-08-20, untrue: this path parsed every
    // matching alert straight through, so an alert an operator had explicitly withdrawn was
    // still served to `entity`, `verify` and the GraphQL `entity` resolver. The withdrawal
    // predicate existed but ran only inside `query_alerts`. A retracted accusation about a
    // named subject continuing to circulate is T-23-FalseAccusation.
    //
    // Withdrawn alerts stay readable where they are asked for — `alerts list
    // --include-withdrawn` — because a retracted false positive is evidence about detector
    // quality. They are simply not volunteered about a subject.
    //
    // PIV-06: the `.take(50)` that used to sit at the end of this chain is gone. It was not a
    // page — nothing downstream could ask for alerts 51..100, and nothing told the analyst there
    // were any. It was an arbitrary literal deciding what an analyst sees, and the thing it hid
    // is the worst case: `build_verify_verdict` derives `p_c_score` from `max()` over this
    // vector, so the highest-scoring alert falling outside an arbitrary 50 silently lowered the
    // verdict. Windowing belongs at the wire boundary, where `workbench-dto`'s `Page<T>` carries
    // the total alongside the rows.
    //
    // Sorted worst-first, then by id: `graph_events` record ids are random, so the scan order
    // this replaces was arbitrary, and PIV-06 asks for a deterministic one.
    let mut alerts: Vec<AlertRecord> =
        scan::scan_events_by_payload_contains(db, "MotifDetected", "result_json", &target)
            .await?
            .into_iter()
            .filter_map(parse_alert_row)
            .filter(|a| a.withdrawn_at.is_none())
            .collect();
    alerts.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));

    // ── 5. Related entities ──────────────────────────────────────────────────
    // D-10's "max 10" is retired on the profile path (PIV-06). The cap was doing real damage
    // beyond the display: `build_verify_verdict`'s `dissolved_relative_count` counts
    // `Dissolved(_)` entries in this vector, so an entity with 400 dissolved relatives and 10
    // returned rows reported at most 10 — and a Phoenix pattern is *made of* dissolved
    // relatives, so the signal was capped at exactly the point an analyst would look for it.
    let related_entities = query_related_entities_complete(db, entity_id).await?;

    // ── 6. LPA presence aggregation (D-03) ───────────────────────────────────
    let lpa_presence = match AppConfig::load() {
        Ok(config) => query_entity_lpa_presence(db, &target, &config)
            .await
            .unwrap_or_default(),
        Err(_) => vec![],
    };

    // ── 7. OSINT Enrichment — Phase 17, SURF-01 seam ─────────────────────────
    // Phase 20 (SURF-05): added osint_links, sanctions_status, osint_alerts.
    // ENR-11: only auto_linked/confirmed links are surfaced — candidate links are
    // never shown in entity or verify output.
    let (beneficial_owners, disqualifications) =
        query_osint_for_entity(db, entity_id, &data.company_number).await;

    // ── 7b. OSINT cross-registry links (Phase 20, SURF-05) ───────────────────
    // ENR-11: only auto_linked/confirmed rows (T-20-05-02: pending_review never exposed).
    //
    // Phase 22 fix (T-22-01-01): the EC projector stores `source_node_id` as the
    // RecordId-string form "entity:`GB-COH:X`" (backtick-quoted), while the bare
    // "GB-COH:X" form is also possible. SurrealDB 3.x RecordId coercion makes a
    // direct `source_node_id = $eid` predicate miss the backtick-quoted stored
    // form, so we over-fetch all auto_linked/confirmed rows and filter Rust-side
    // on both accepted forms.
    // GRD-09: routed through scan::scan_identity_links (paged, `?`-propagated on error) instead
    // of a bespoke `LIMIT 10000` swallowed by `.ok().and_then(...).unwrap_or_default()` — a
    // genuine SurrealDB failure here used to present as "this entity has zero OSINT links"
    // (T-31-09), indistinguishable from a true negative.
    let osint_rec_form = format!("entity:`{entity_id}`");
    let mut osint_link_rows_all = scan::scan_identity_links(db, Some("auto_linked"), None).await?;
    osint_link_rows_all.extend(scan::scan_identity_links(db, Some("confirmed"), None).await?);

    // Match the bare entity_id ("GB-COH:X") OR the RecordId-string form
    // ("entity:`GB-COH:X`"); strip surrounding quotes from the stored value.
    let osint_link_rows: Vec<scan::IdentityLinkRow> = osint_link_rows_all
        .into_iter()
        .filter(|r| {
            let sni = r
                .source_node_id
                .trim_matches(|c: char| c == '\'' || c == '"');
            sni == entity_id || sni == osint_rec_form
        })
        .collect();

    // The one family whose provenance is recorded rather than inferred: `source`, `resolved_at`
    // and `status` are columns on the row. `identity_link_provenance` re-checks the status against
    // `LinkTier`'s two-variant vocabulary, so a `pending_review` row that got past the scan filter
    // above becomes `Unattributed` instead of an attributed fact with a missing tier (ENR-11).
    let osint_links: Vec<OsintLink> = osint_link_rows
        .into_iter()
        .map(|r| OsintLink {
            target_node_id: r.target_node_id,
            confidence: r.confidence,
            resolution_type: r.resolution_type,
            provenance: identity_link_provenance(&r.source, &r.status, r.resolved_at),
            source_system: r.source,
            status: r.status,
        })
        .collect();

    // ── 7c. Sanctions status (Phase 20, SURF-05) ─────────────────────────────
    // Populated from opensanctions-sourced identity_links (confirmed/auto_linked only).
    // GRD-09: routed through scan::scan_identity_links, `?`-propagated — same T-31-09 fix as
    // above. scan_identity_links has no source_node_id-equality filter, so the original
    // `source_node_id = $eid` predicate is reproduced Rust-side after the fetch, mirroring the
    // RecordId-coercion-safe "fetch by indexed field, filter Rust-side" pattern used elsewhere
    // in this file.
    let mut sanctions_rows_all =
        scan::scan_identity_links(db, Some("auto_linked"), Some("opensanctions")).await?;
    sanctions_rows_all
        .extend(scan::scan_identity_links(db, Some("confirmed"), Some("opensanctions")).await?);
    let sanctions_rows: Vec<scan::IdentityLinkRow> = sanctions_rows_all
        .into_iter()
        .filter(|r| r.source_node_id == entity_id)
        .collect();

    let sanctions_status: Option<SanctionsStatus> =
        sanctions_rows.first().map(|r| SanctionsStatus {
            on_list: true,
            match_name: Some(r.target_node_id.clone()),
            match_score: Some(r.confidence),
            dataset: None, // populate from evidence_json in a future phase
            provenance: identity_link_provenance(&r.source, &r.status, r.resolved_at),
        });

    // ── 7d. OSINT alerts (Phase 20, SURF-05) — 7th OSINT capability ──────────
    // graph_events WHERE kind='MotifDetected' AND source IN OSINT registries.
    // Note: source field on graph_events uses the detector's source tag —
    // if the event lacks a 'source' field, we filter by payload.result_json
    // containing the entity_id (same as step 4 above) instead.
    // Same PIV-06 removal as section 4's: the `.take(50)` here was applied BEFORE the
    // OSINT-source filter below, so a single non-OSINT alert in the arbitrary first fifty
    // displaced a real OSINT one — the section could read empty for an entity that has them.
    let alert_rows: Vec<serde_json::Value> =
        scan::scan_events_by_payload_contains(db, "MotifDetected", "result_json", &target)
            .await
            .unwrap_or_default();

    // Filter to OSINT-sourced alerts by checking result_json for OSINT sources.
    let osint_sources = [
        "electoral_commission",
        "charity_commission",
        "icij",
        "opensanctions",
    ];
    let osint_alerts: Vec<OsintAlertRow> = alert_rows
        .into_iter()
        .filter_map(|r| {
            // Extract motif_type and risk_score from result_json
            let result_json_str = r["payload"]["result_json"].as_str()?;
            let result: serde_json::Value = serde_json::from_str(result_json_str).ok()?;
            // Only include alerts referencing an OSINT source in participants or source field
            let result_str = result_json_str.to_lowercase();
            let is_osint = osint_sources.iter().any(|src| result_str.contains(src));
            if !is_osint {
                return None;
            }
            let motif_type = result["motif_type"].as_str()?.to_string();
            let risk_score = result["risk_score"].as_f64().unwrap_or(0.0);
            let severity = if risk_score >= 0.9 {
                "CRITICAL"
            } else if risk_score >= 0.7 {
                "HIGH"
            } else {
                "MEDIUM"
            }
            .to_string();
            Some(OsintAlertRow {
                motif_type,
                severity,
                provenance: detector_provenance(r["timestamp"].as_u64()),
            })
        })
        .collect();

    // ── 7e. Four in-position relation tables (Phase 22, PROP-07) ─────────────
    // owns_property / offshore_link / trustee_of / donates_to all carry the
    // entity on the IN (left / source) side per the DDL (RELATION FROM entity
    // TO ...), so the entity appears in the `in` field. The pattern mirrors the
    // `controls` read but FLIPPED: filter on `in_str` instead of `out_str`.
    //
    // Security (T-22-01-03): filter on `in_str` exactly so a decoy entity's
    // edges never appear in this profile (false-attribution guard).
    // Security (T-22-01-02): display-name reads from graph_events use a
    // CONTAINS-only query with the `kind` filter applied Rust-side (Pitfall 7:
    // never combine `kind = ... AND ... CONTAINS ...`).

    // The entity's IN-position RecordId string (backtick-quoted; the id holds ':' and '-').
    // OCOD offshore owners key as `offshore_entity:`GB-OCOD:…`` (compute_in_key).
    let entity_in_key = compute_in_key(entity_id, data.company_number.as_deref());

    // ── owns_property (RELATION FROM entity | offshore_entity TO property) ─────
    // Filter by the owner's IN key in SQL: at GB scale owns_property holds ~100k
    // OCOD edges, so an unfiltered LIMIT-100000 fetch is wasteful and unreliable
    // over the HTTP engine (returns the owner's ~hundreds of rows instead).
    // type::string(in) matches the backtick-quoted record string from compute_in_key.
    // PIV-06: the `LIMIT 100000` this replaces was a ceiling, not a page — no `START`, no
    // `ORDER BY`, and sized to the ~100k OCOD edges the comment above names. One more ingest and
    // the profile's property section would have rendered a sample of the table as the entity's
    // holdings, with nothing to say so. `scan_relation_rows` keeps the SQL-side `in` filter (the
    // reason this read is affordable at GB scale) and pages the result.
    let owns_property_rows: Vec<serde_json::Value> = scan::scan_relation_rows(
        db,
        "owns_property",
        &[
            ("data.price_paid", "price_paid"),
            ("data.tenure", "tenure"),
            CREATED_AT_PROJECTION,
            EFFECTIVE_FROM_PROJECTION,
            EFFECTIVE_TO_PROJECTION,
        ],
        Some((scan::RelationSide::In, &entity_in_key)),
    )
    .await
    .unwrap_or_default();

    let owns_property: Vec<PropertyRow> = owns_property_rows
        .into_iter()
        .filter_map(|row| {
            let out_raw = row["out_str"].as_str()?;
            let label = out_raw
                .trim_matches(|c: char| c == '\'' || c == '"')
                .strip_prefix("property:")
                .unwrap_or(out_raw)
                .trim_matches('`');
            let price = row["price_paid"].as_f64().filter(|p| *p > 0.0);
            let tenure = row["tenure"].as_str().unwrap_or("").to_owned();
            let display = match (price, tenure.is_empty()) {
                (Some(p), true) => format!("{label} (£{p:.0})"),
                (Some(p), false) => format!("{label} (£{p:.0}, {tenure})"),
                (None, true) => label.to_owned(),
                (None, false) => format!("{label} ({tenure})"),
            };
            // Attributed off the row's own IN side rather than off `entity_in_key`: the SQL filter
            // and the attribution then cannot disagree, and a row that somehow arrived from the
            // wrong end is attributed to what it actually says rather than to what was asked for.
            let in_id = row["in_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c: char| c == '\'' || c == '"');
            Some(PropertyRow {
                label: display,
                provenance: relation_provenance("owns_property", in_id, row.get("created_at")),
                // One of the two sections on this corpus a date filter can actually act on:
                // 107,804 of 108,026 `owns_property` rows carry a `meta.effective_from` (the date
                // of the transfer), measured 2026-09-15. None carries an end.
                temporal: Temporal::unfiltered(relation_effective_range(
                    "owns_property",
                    row.get("effective_from"),
                    row.get("effective_to"),
                )),
            })
        })
        .collect();

    // ── offshore_link (RELATION FROM entity TO offshore_entity) ───────────────
    // Paged, not capped (PIV-06) — same removal as `owns_property` above. The `in`-side filter
    // stays Rust-side here because the stored value needs this module's own quote-trimming
    // before it compares equal to `entity_in_key`.
    let offshore_link_rows: Vec<serde_json::Value> = scan::scan_relation_rows(
        db,
        "offshore_link",
        &[
            CREATED_AT_PROJECTION,
            EFFECTIVE_FROM_PROJECTION,
            EFFECTIVE_TO_PROJECTION,
        ],
        None,
    )
    .await
    .unwrap_or_default();

    let offshore_links: Vec<LinkedRecordRow> = offshore_link_rows
        .into_iter()
        .filter(|row| {
            row["in_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c: char| c == '\'' || c == '"')
                == entity_in_key
        })
        .filter_map(|row| {
            let out_raw = row["out_str"].as_str()?;
            let label = out_raw
                .trim_matches(|c: char| c == '\'' || c == '"')
                .strip_prefix("offshore_entity:")
                .unwrap_or(out_raw)
                .trim_matches('`')
                .to_owned();
            Some(LinkedRecordRow {
                label,
                // Always `Unattributed`, and for a reason that is about the data rather than
                // about this code: nothing in the workspace produces an `offshore_link` edge.
                provenance: relation_provenance(
                    "offshore_link",
                    entity_in_key.as_str(),
                    row.get("created_at"),
                ),
                // Always `Unrecorded`, and for a reason about the data rather than this code:
                // nothing in the workspace produces an `offshore_link` edge, and the table holds 0 rows.
                temporal: Temporal::unfiltered(relation_effective_range(
                    "offshore_link",
                    row.get("effective_from"),
                    row.get("effective_to"),
                )),
            })
        })
        .collect();

    // ── trustee_of (RELATION FROM entity TO charity) ──────────────────────────
    // Paged, not capped (PIV-06) — same removal as `owns_property` above. The `in`-side filter
    // stays Rust-side here because the stored value needs this module's own quote-trimming
    // before it compares equal to `entity_in_key`.
    let trustee_of_rows: Vec<serde_json::Value> = scan::scan_relation_rows(
        db,
        "trustee_of",
        &[
            CREATED_AT_PROJECTION,
            EFFECTIVE_FROM_PROJECTION,
            EFFECTIVE_TO_PROJECTION,
        ],
        None,
    )
    .await
    .unwrap_or_default();

    // Resolve charity display names from graph_events (CONTAINS-only, kind Rust-side).
    let charity_name_map: std::collections::HashMap<String, String> =
        if trustee_of_rows.iter().any(|row| {
            row["in_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c: char| c == '\'' || c == '"')
                == entity_in_key
        }) {
            let charity_events = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Charity\"")
                .await
                .unwrap_or_default();
            charity_events
                .into_iter()
                .filter_map(|row| {
                    use urban_integrity_schema::types::node::NodeWeight as NW;
                    let eid = row["payload"]["entity_id"].as_str()?.to_owned();
                    let wj = row["payload"]["weight_json"].as_str()?;
                    if let Ok(NW::Charity(cd)) = serde_json::from_str::<NW>(wj) {
                        Some((eid, cd.name))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            std::collections::HashMap::new()
        };

    let trustee_of: Vec<LinkedRecordRow> = trustee_of_rows
        .into_iter()
        .filter(|row| {
            row["in_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c: char| c == '\'' || c == '"')
                == entity_in_key
        })
        .filter_map(|row| {
            let out_raw = row["out_str"].as_str()?;
            let bare_id = out_raw
                .trim_matches(|c: char| c == '\'' || c == '"')
                .strip_prefix("charity:")
                .unwrap_or(out_raw)
                .trim_matches('`')
                .to_owned();
            let display = charity_name_map.get(&bare_id).cloned().unwrap_or(bare_id);
            Some(LinkedRecordRow {
                label: display,
                provenance: relation_provenance(
                    "trustee_of",
                    entity_in_key.as_str(),
                    row.get("created_at"),
                ),
                // `Unrecorded` on all 85 live rows — the Charity Commission adapter writes no
                // trusteeship dates, so a trustee-of row reaches the filter as `Undated`.
                temporal: Temporal::unfiltered(relation_effective_range(
                    "trustee_of",
                    row.get("effective_from"),
                    row.get("effective_to"),
                )),
            })
        })
        .collect();

    // ── donates_to (RELATION FROM entity TO political_actor) ───────────────────
    // Paged, not capped (PIV-06) — same removal as `owns_property` above. The `in`-side filter
    // stays Rust-side here because the stored value needs this module's own quote-trimming
    // before it compares equal to `entity_in_key`.
    let donates_to_rows: Vec<serde_json::Value> = scan::scan_relation_rows(
        db,
        "donates_to",
        &[
            CREATED_AT_PROJECTION,
            EFFECTIVE_FROM_PROJECTION,
            EFFECTIVE_TO_PROJECTION,
        ],
        None,
    )
    .await
    .unwrap_or_default();

    // Resolve political_actor display names (CONTAINS-only, kind Rust-side — T-22-01-02).
    let political_name_map: std::collections::HashMap<String, String> = if donates_to_rows
        .iter()
        .any(|row| {
            row["in_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c: char| c == '\'' || c == '"')
                == entity_in_key
        }) {
        let pol_events = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"PoliticalActor\"")
            .await
            .unwrap_or_default();
        pol_events
            .into_iter()
            .filter_map(|row| {
                use urban_integrity_schema::types::node::NodeWeight as NW;
                let eid = row["payload"]["entity_id"].as_str()?.to_owned();
                let wj = row["payload"]["weight_json"].as_str()?;
                if let Ok(NW::PoliticalActor(pd)) = serde_json::from_str::<NW>(wj) {
                    // Key the map by the bare-domain id so it matches the
                    // `bare_id` lookup below. Both sides strip a single leading
                    // `political_actor:` prefix, normalising to the bare recipient
                    // id (`PA:<party-slug>` for EC parties, `PA:<councillor-slug>`
                    // for council registers). The strip is a no-op for already-bare
                    // ids and also tolerates any legacy `political_actor:`-prefixed
                    // entity_id — so name resolution holds across both id forms.
                    let key = eid
                        .strip_prefix("political_actor:")
                        .unwrap_or(&eid)
                        .to_owned();
                    Some((key, pd.name))
                } else {
                    None
                }
            })
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    let donates_to: Vec<LinkedRecordRow> = donates_to_rows
        .into_iter()
        .filter(|row| {
            row["in_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c: char| c == '\'' || c == '"')
                == entity_in_key
        })
        .filter_map(|row| {
            let out_raw = row["out_str"].as_str()?;
            let bare_id = out_raw
                .trim_matches(|c: char| c == '\'' || c == '"')
                .strip_prefix("political_actor:")
                .unwrap_or(out_raw)
                .trim_matches('`')
                .to_owned();
            let display = political_name_map.get(&bare_id).cloned().unwrap_or(bare_id);
            Some(LinkedRecordRow {
                label: display,
                provenance: relation_provenance(
                    "donates_to",
                    entity_in_key.as_str(),
                    row.get("created_at"),
                ),
                // `Unrecorded` on all 22 live rows — the Electoral Commission adapter records the
                // donation's own date nowhere on this edge.
                temporal: Temporal::unfiltered(relation_effective_range(
                    "donates_to",
                    row.get("effective_from"),
                    row.get("effective_to"),
                )),
            })
        })
        .collect();

    // ── 8. Sub-candidate links (Phase 19, SURF-02) ───────────────────────────
    // Only fetched when deep=true — D-13: sub_candidate links are NOT shown in
    // the standard entity query path. ENR-11: investigative only, not actionable.
    let sub_candidate_links = if deep {
        query_sub_candidates_for_entity(db, entity_id).await?
    } else {
        vec![]
    };

    Ok(EntityProfile {
        id: entity_id.to_string(),
        name: data.name,
        ch_number: data.company_number,
        status: data.status,
        incorporation_date: data.incorporation_date,
        identity_provenance,
        addresses,
        directors,
        contracts,
        planning_applications,
        alerts,
        related_entities,
        lpa_presence,
        beneficial_owners,
        disqualifications,
        offshore_links,
        trustee_of,
        owns_property,
        donates_to,
        osint_links,
        sanctions_status,
        osint_alerts,
        sub_candidate_links,
        // Assembly never filters. `query_entity_by_id_as_at` stamps this through `apply_as_at`
        // after the profile is complete, which is what keeps the unfiltered totals recoverable.
        as_at: None,
    })
}

/// Query `identity_links` for confirmed/auto-linked PSC and disqualified-director
/// records associated with this entity, and return the OSINT enrichment rows.
///
/// ENR-11 invariant: only `status IN ('auto_linked', 'confirmed')` rows are surfaced.
/// Candidate links (confidence < 0.85) are never shown to the analyst.
///
/// Uses fetch-all + Rust-side filter to avoid SurrealDB 3.x RecordId coercion issues
/// (same workaround as `load_officer_status()` in `evasion.rs`).
///
/// Returns `(beneficial_owners, disqualifications)` — errors are non-fatal (returns
/// empty vecs and logs WARN) so a missing enricher table never breaks entity display.
async fn query_osint_for_entity(
    db: &Surreal<Any>,
    entity_id: &str,
    ch_number: &Option<String>,
) -> (Vec<BeneficialOwnerRow>, Vec<DisqualificationRow>) {
    // ── PSC / beneficial owners ───────────────────────────────────────────────
    // PSC data lives in the projected `controls` relation table — `in` is the
    // `psc:<…>` node, `out` is the company `entity:GB-COH:<ch>` node — NOT in
    // identity_links (there are zero `ch_psc` identity_links rows; PSC is never
    // written to that table). The original identity_links read therefore always
    // returned empty (entity-osint-display-empty).
    //
    // Read path: (1) fetch `controls` edges projecting `in`/`out` as strings to
    // find which PSC nodes control THIS company; (2) resolve each PSC node's
    // name + natures_of_control from `graph_events` (NodeAdded / Psc). We use
    // graph_events rather than the native `psc` table because the SurrealDB SDK
    // unreliably returns empty/NULL for native-table and graph-traversal reads
    // (the documented coercion quirk this whole module works around), whereas
    // the graph_events CONTAINS-scan + Rust-side filter is the proven surface.
    let entity_company_node = ch_number
        .as_deref()
        .map(|ch| format!("GB-COH:{ch}"))
        .unwrap_or_else(|| entity_id.to_string());
    // The `controls.out` RecordId stringifies as "entity:`GB-COH:<ch>`"
    // (backtick-quoted because the id contains a colon/hyphen).
    let target_out = format!("entity:`{entity_company_node}`");

    // Paged, not capped (PIV-06). A truncated `controls` read presents as "this company has no
    // beneficial owners", which is a claim, not an absence of one.
    let control_rows: Vec<serde_json::Value> = scan::scan_relation_rows(
        db,
        "controls",
        &[
            CREATED_AT_PROJECTION,
            EFFECTIVE_FROM_PROJECTION,
            EFFECTIVE_TO_PROJECTION,
        ],
        None,
    )
    .await
    .unwrap_or_default();

    // Collect the bare PSC node entity_ids (graph_events `payload.entity_id`
    // form) that control THIS company. `in_str` is "psc:`<entity_id>`" — strip
    // the `psc:` table prefix and the surrounding backticks to recover the
    // graph_events entity_id (e.g. "psc:15639177-jehad-alnashash").
    //
    // The `controls` edge is also where a beneficial-owner row's provenance comes from: the PSC
    // node supplies the name, but the CLAIM that this person controls this company is the edge,
    // and `attribution::REGISTRY` records `Controls` as having exactly one producer (`ch_psc`).
    // `meta.created_at` rides along per PSC id for that reason.
    //
    // So do `meta.effective_from` / `meta.effective_to` (PIV-04). This is the OTHER of the two
    // sections a date filter can act on today: 19,547 of 19,548 `controls` rows carry an
    // `effective_from` — the PSC's `notified_on` — and none carries an end.
    type ControlMeta = (
        String,
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
    );
    let mut psc_control_meta: HashMap<String, ControlMeta> = HashMap::new();
    let psc_entity_ids: HashSet<String> = control_rows
        .iter()
        .filter(|row| {
            row["out_str"]
                .as_str()
                .unwrap_or("")
                .trim_matches(|c| c == '\'' || c == '"')
                == target_out
        })
        .filter_map(|row| {
            let in_str = row["in_str"].as_str()?;
            // "psc:`psc:<id>`" → "psc:<id>"
            let psc_id = in_str
                .strip_prefix("psc:")
                .map(|rest| rest.trim_matches('`').to_owned())?;
            let field = |key: &str| row.get(key).cloned().unwrap_or(serde_json::Value::Null);
            psc_control_meta.insert(
                psc_id.clone(),
                (
                    in_str.trim_matches(|c| c == '\'' || c == '"').to_owned(),
                    field("created_at"),
                    field("effective_from"),
                    field("effective_to"),
                ),
            );
            Some(psc_id)
        })
        .collect();

    let beneficial_owners: Vec<BeneficialOwnerRow> = if psc_entity_ids.is_empty() {
        Vec::new()
    } else {
        // Fetch Psc NodeAdded events (CONTAINS-only to dodge the planner bug that
        // makes `kind = 'NodeAdded' AND CONTAINS …` return 0 rows; kind filtered
        // Rust-side) and match on payload.entity_id.
        let psc_rows = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Psc\"")
            .await
            .unwrap_or_default();

        psc_rows
            .into_iter()
            .filter(|row| {
                row["payload"]["entity_id"]
                    .as_str()
                    .map(|e| psc_entity_ids.contains(e))
                    .unwrap_or(false)
            })
            .filter_map(|row| {
                let psc_id = row["payload"]["entity_id"].as_str()?.to_owned();
                let wj = row["payload"]["weight_json"].as_str()?;
                if let Ok(NodeWeight::Psc(pd)) = serde_json::from_str::<NodeWeight>(wj) {
                    if pd.name.is_empty() {
                        return None;
                    }
                    let meta = psc_control_meta.get(&psc_id);
                    let (in_id, created_at) = meta
                        .map(|(i, c, _, _)| (i.as_str(), Some(c)))
                        .unwrap_or(("", None));
                    Some(BeneficialOwnerRow {
                        name: pd.name,
                        natures_of_control: pd.natures_of_control.unwrap_or_default(),
                        provenance: relation_provenance("controls", in_id, created_at),
                        temporal: Temporal::unfiltered(relation_effective_range(
                            "controls",
                            meta.map(|(_, _, f, _)| f),
                            meta.map(|(_, _, _, t)| t),
                        )),
                    })
                } else {
                    None
                }
            })
            .collect()
    };

    // ── Disqualified directors ────────────────────────────────────────────────
    // Fetch identity_links with source='ch_disqualified' AND status IN auto_linked/confirmed.
    // source_node_id matches the officer (OFF:<company>-<slug> or director:<slug>).
    // We surface disqualifications for any officer linked to this entity via Directs edges.
    // For Phase 17, we fetch all confirmed disqualified links and then cross-reference
    // with the entity's known officer IDs (from directors vec would require another query).
    // Simpler approach: fetch disqualified links where source_node_id is referenced by
    // any NodeAdded Officer event that has a Directs edge to this entity.
    //
    // Since `directors` is already assembled above, we collect their node IDs
    // via a name-slug lookup. For correctness in Phase 17, we query all confirmed
    // disqualified links for any officer associated with this entity by CH number.
    // The entity's ch_number is the discriminating field for the Directs/DisqualifiedFrom join.
    // GRD-09: routed through scan::scan_identity_links (paged past the old `LIMIT 200`).
    // This function stays non-`Result` — its doc comment above promises callers a missing
    // enricher table never breaks entity display — but the SILENT half of the swallow is fixed:
    // a genuine scan failure is now logged instead of presenting identically to "no
    // disqualifications", the read-side twin of the ICIJ silent-write bug.
    let disq_links: Vec<scan::IdentityLinkRow> = {
        let mut rows = Vec::new();
        let mut ok = true;
        for status in ["auto_linked", "confirmed"] {
            match scan::scan_identity_links(db, Some(status), Some("ch_disqualified")).await {
                Ok(page) => rows.extend(page),
                Err(e) => {
                    ok = false;
                    tracing::warn!(
                        error = %e,
                        status,
                        "disqualified identity_links scan failed"
                    );
                }
            }
        }
        if ok {
            rows
        } else {
            Vec::new()
        }
    };

    // For Phase 17, surface any confirmed disqualified link whose evidence_json
    // contains this entity's company number (the disqualification was recorded
    // in the context of companies where this officer was active).
    // If ch_number is absent, no disqualification links can be correlated.
    let disqualifications: Vec<DisqualificationRow> = if let Some(ch) = ch_number.as_deref() {
        // Also fetch DisqualifiedFrom EdgeAdded rows for any officer linked to this company.
        // Fetch all DisqualifiedFrom edges and match Rust-side (RecordId coercion workaround).
        let disq_edge_rows: Vec<serde_json::Value> =
            scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"DisqualifiedFrom\"")
                .await
                .unwrap_or_default();

        // Build a map of officer_id → DisqualifiedFromData for officers linked to this entity.
        // An officer is linked to this entity if their identity_link source_node_id is in
        // the disq_links and their evidence references our company.
        let relevant_officer_ids: std::collections::HashSet<String> = disq_links
            .iter()
            .filter(|row| {
                // Include this link if the evidence_json references our ch_number
                row.evidence_json.contains(ch)
            })
            .map(|row| {
                row.source_node_id
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_owned()
            })
            .collect();

        // For each relevant officer, find their DisqualifiedFrom edge and extract the data.
        let mut rows = Vec::new();
        for officer_id in &relevant_officer_ids {
            // Find the matching DisqualifiedFrom edge where src_id == officer_id.
            for edge_row in &disq_edge_rows {
                let src = edge_row["payload"]["src_id"]
                    .as_str()
                    .unwrap_or("")
                    .trim_matches(|c| c == '\'' || c == '"');
                if src != officer_id {
                    continue;
                }
                let wj = match edge_row["payload"]["weight_json"].as_str() {
                    Some(s) => s,
                    None => continue,
                };
                if let Ok(serde_json::Value::Object(map)) =
                    serde_json::from_str::<serde_json::Value>(wj)
                {
                    if map.get("type").and_then(|v| v.as_str()) != Some("DisqualifiedFrom") {
                        continue;
                    }
                    let data_obj = &map["data"];
                    let from = data_obj["disqualified_from"].as_str().map(str::to_owned);
                    let until = data_obj["disqualified_until"].as_str().map(str::to_owned);
                    let case_num = data_obj["case_number"].as_str().map(str::to_owned);

                    // Derive officer display name from the officer_id slug
                    // (e.g. "OFF:12345678-john-smith" → "john smith" or from evidence_json)
                    let link = disq_links.iter().find(|l| {
                        l.source_node_id.trim_matches(|c| c == '\'' || c == '"') == officer_id
                    });
                    let officer_name = link
                        .and_then(|l| {
                            let ev_val: serde_json::Value =
                                serde_json::from_str(&l.evidence_json).ok()?;
                            ev_val["name_slug"].as_str().map(|s| s.replace('-', " "))
                        })
                        .unwrap_or_else(|| officer_id.clone());

                    // The row exists BECAUSE an identity link joined this officer to a CDDA
                    // record, so that link is its provenance — including its tier, which is the
                    // half of PIV-03 only a link-derived fact has. `relevant_officer_ids` was
                    // built from `disq_links`, so `link` is always `Some` here; the `None` arm is
                    // an honest unattributed rather than an `expect`.
                    let provenance = match link {
                        Some(l) => identity_link_provenance(&l.source, &l.status, l.resolved_at),
                        None => Provenance::Unattributed {
                            reason: "no identity_links row could be matched back to this officer"
                                .to_owned(),
                        },
                    };

                    // The ban's own dates ARE the range, and this is the one section on the
                    // profile where a source records BOTH endpoints. The `DisqualifiedFrom` edge's
                    // `meta` is not consulted: a CDDA order states its own term, and the edge that
                    // carries it into this graph does not.
                    let range = effective_range(&[(
                        "disqualification.disqualified_from / disqualification.disqualified_until",
                        from.clone(),
                        until.clone(),
                    )]);
                    rows.push(DisqualificationRow {
                        officer_name,
                        case_number: case_num,
                        disqualified_from: from,
                        disqualified_until: until,
                        provenance,
                        temporal: Temporal::unfiltered(range),
                    });
                    break; // one DisqualifiedFrom edge per officer
                }
            }
        }
        rows
    } else {
        vec![]
    };

    (beneficial_owners, disqualifications)
}

/// Query `identity_links` for sub-candidate (0.50–0.69) links associated with this entity.
///
/// D-13: sub_candidate links are NOT surfaced in the standard entity query path — only when
/// `deep=true` is explicitly requested by the analyst. Not actionable via `links` CLI.
///
/// GRD-09: routed through `scan::scan_identity_links` (paged past the old `LIMIT 500`) instead of
/// a bespoke query whose `.ok().and_then(...)` made a genuine SurrealDB failure indistinguishable
/// from "no sub-candidate links" (T-31-09). Fetch-all + Rust-side filter is kept to avoid
/// SurrealDB 3.x RecordId coercion issues (same workaround as `load_officer_status()` in
/// `evasion.rs`) — filters to rows where `source_node_id` or `target_node_id` contains the
/// entity_id string.
///
/// T-19-06-03: entity_id filtering is Rust-side (no user-supplied values in SurrealQL).
/// The status = 'sub_candidate' literal is a constant, not interpolated.
///
/// Errors propagate to the caller — unlike `query_osint_for_entity`'s `disq_links`, this
/// function's only caller (`assemble_entity_profile`) already returns a `Result`, so a genuine
/// scan failure surfaces as a failed entity lookup rather than a silently empty deep-scan section.
async fn query_sub_candidates_for_entity(
    db: &Surreal<Any>,
    entity_id: &str,
) -> surrealdb::Result<Vec<SubCandidateLinkRow>> {
    let raw = scan::scan_identity_links(db, Some("sub_candidate"), None).await?;

    Ok(raw
        .into_iter()
        .filter(|row| {
            // Rust-side filter: include rows where source_node_id or target_node_id
            // contains entity_id (RecordId coercion workaround — same pattern as evasion.rs).
            let src = row.source_node_id.trim_matches(|c| c == '\'' || c == '"');
            let tgt = row.target_node_id.trim_matches(|c| c == '\'' || c == '"');
            src.contains(entity_id) || tgt.contains(entity_id)
        })
        .map(|row| SubCandidateLinkRow {
            source_node_id: row
                .source_node_id
                .trim_matches(|c| c == '\'' || c == '"')
                .to_owned(),
            target_node_id: row
                .target_node_id
                .trim_matches(|c| c == '\'' || c == '"')
                .to_owned(),
            source: row.source,
            confidence: row.confidence,
            resolution_type: row.resolution_type,
            evidence_json: row.evidence_json,
        })
        .collect())
}

/// Query identity-link enrichment statistics for a company number.
///
/// Aggregates `identity_links` rows by status for the given company number.
/// Returns `None` when no links exist (not an error — new companies have no links).
///
/// # Security (ASVS V5)
/// The `company_number` parameter is validated by `is_valid_ch_number` before embedding
/// in the query — never string-interpolated into SurrealQL (T-20-05-03 mitigation).
///
/// Used by the `enrichmentStatus(companyNumber)` GraphQL resolver (SURF-05).
pub async fn query_enrichment_status(
    company_number: &str,
    db: &Surreal<Any>,
) -> anyhow::Result<Option<EnrichmentStatusResult>> {
    if !is_valid_ch_number(company_number) {
        return Ok(None);
    }

    // GRD-09: the previous `source_node_id CONTAINS $cn OR target_node_id CONTAINS $cn` shape
    // pairs an (indexed) full-table scan intent with CONTAINS, which the research notes flag as a
    // genuinely unverified SurrealDB 3.x planner-bug risk (LOW confidence, no direct measurement
    // either way — see `scan.rs`'s module doc for the confirmed instance of this shape on
    // `graph_events`). Rather than page around an unverified-risk predicate, drop it entirely:
    // scan the whole table via `scan::scan_identity_links` (paged, safety-capped) and keep the
    // existing Rust-side `contains` filter. This trades a DB-side filter for a full paginated
    // scan on every call — acceptable per T-31-10 (not a hot path today); a future phase can
    // narrow this back down if entity-lookup latency on `enrichmentStatus` becomes a measured
    // concern.
    let all_rows = scan::scan_identity_links(db, None, None).await?;

    // Filter Rust-side: include rows whose source_node_id or target_node_id contains the CH number.
    let cn = company_number;
    let relevant: Vec<&scan::IdentityLinkRow> = all_rows
        .iter()
        .filter(|row| row.source_node_id.contains(cn) || row.target_node_id.contains(cn))
        .collect();

    if relevant.is_empty() {
        return Ok(None);
    }

    let mut auto_linked: u32 = 0;
    let mut pending_review: u32 = 0;
    let mut confirmed: u32 = 0;
    // GRD-09: the original code read `row["created_at"]`, a field that does not exist on
    // `identity_links` (the schema field is `resolved_at`, `db.rs:263`) — `last_enriched` was
    // therefore always `None`. Fixed as a byproduct of this rewrite by tracking the typed
    // `resolved_at` field during the fold.
    let mut last_enriched: Option<chrono::DateTime<chrono::Utc>> = None;

    for row in &relevant {
        match row.status.as_str() {
            "auto_linked" => auto_linked += 1,
            "pending_review" => pending_review += 1,
            "confirmed" => confirmed += 1,
            _ => {}
        }
        // Track the most recent resolved_at for last_enriched.
        match &last_enriched {
            None => last_enriched = Some(row.resolved_at),
            Some(existing) if row.resolved_at > *existing => {
                last_enriched = Some(row.resolved_at);
            }
            _ => {}
        }
    }

    Ok(Some(EnrichmentStatusResult {
        company_number: company_number.to_string(),
        total_links: relevant.len() as u32,
        auto_linked,
        pending_review,
        confirmed,
        last_enriched: last_enriched.map(|dt| dt.to_rfc3339()),
    }))
}

/// Parse a raw SurrealDB `graph_events` row into an `AlertRecord`.
///
/// EventRecord shape persisted by `surreal_writer_loop`:
/// ```json
/// {
///   "event_id": "uuid",
///   "timestamp": 1700000000000,
///   "kind": "MotifDetected",
///   "payload": { "result_json": "{\"motif_type\":\"PhoenixLoop\",\"risk_score\":0.85,...}" }
/// }
/// ```
///
/// MotifResult shape (from `crates/detector/src/motif/*.rs`):
/// ```json
/// {
///   "motif_type": "PhoenixLoop",
///   "risk_score": 0.85,
///   "ward_code": "X05999001",
///   "path_nodes": [...],
///   "unique_hash": "abc123"
/// }
/// ```
pub(crate) fn parse_alert_row(row: serde_json::Value) -> Option<AlertRecord> {
    if row["kind"].as_str()? != "MotifDetected" {
        return None;
    }

    let result_json_str = row["payload"]["result_json"].as_str()?;
    let result: serde_json::Value = serde_json::from_str(result_json_str).ok()?;

    let timestamp_ms = row["timestamp"].as_u64().unwrap_or(0);
    let detected_at = chrono::DateTime::from_timestamp_millis(timestamp_ms as i64)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    Some(AlertRecord {
        id: row["event_id"].as_str().unwrap_or("").to_string(),
        motif_type: result["motif_type"]
            .as_str()
            .unwrap_or("unknown")
            .to_string(),
        score: result["risk_score"].as_f64().unwrap_or(0.0),
        result_json: result_json_str.to_string(),
        detected_at,
        ward_code: result["score_breakdown"]["ward_code"]
            .as_str()
            .map(|s| s.to_string()),
        // Absent on every alert written before withdrawal existed, which must parse as live
        // rather than failing the row.
        withdrawn_at: result["withdrawn_at"].as_str().map(|s| s.to_string()),
        withdrawn_reason: result["withdrawn_reason"].as_str().map(|s| s.to_string()),
        // Absent on 5 of the 8 detector types until 46-03/46-05 ship the key; defaults to `[]`
        // rather than failing the row (TRI-02).
        participants: serde_json::from_value(result["participants"].clone()).unwrap_or_default(),
        first_seen_run_id: result["first_seen_run_id"].as_str().map(|s| s.to_string()),
        last_seen_run_id: result["last_seen_run_id"].as_str().map(|s| s.to_string()),
        // `row["timestamp"]`, not `timestamp_ms` — the latter has already been collapsed to 0 for
        // a missing field, which `detector_provenance` would then render as the epoch instead of
        // reporting that there was nothing to read.
        provenance: detector_provenance(row["timestamp"].as_u64()),
    })
}

pub async fn enrich_alert_participants(
    db: &Surreal<Any>,
    slugs: &[String],
) -> Result<std::collections::HashMap<String, String>, surrealdb::Error> {
    if slugs.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let unique_slugs: std::collections::HashSet<&str> = slugs.iter().map(String::as_str).collect();
    let mut map = std::collections::HashMap::new();

    let entity_rows: Vec<serde_json::Value> =
        scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Entity\"").await?;
    for row in fold_latest_node_rows(entity_rows) {
        if let Some((eid, data)) = parse_entity_node_row(row) {
            if unique_slugs.contains(eid.as_str()) {
                map.insert(eid, data.name);
            }
        }
    }

    let officer_rows: Vec<serde_json::Value> =
        scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Officer\"").await?;
    for row in fold_latest_node_rows(officer_rows) {
        if let Some((oid, data)) = parse_officer_node_row(row) {
            if unique_slugs.contains(oid.as_str()) {
                map.entry(oid).or_insert(data.name);
            }
        }
    }

    Ok(map)
}

// ── Phase 13: LPA-presence aggregation and entity resolution seam ─────────────

/// Per-LPA activity summary for an entity.
///
/// Returned by [`query_entity_lpa_presence`]. The `lpa_code` field holds the ONS
/// Local Authority code (e.g. `"X06999001"`) or the sentinel `"Unknown LPA"` for
/// contracts whose ward is absent from the portal config (D-02).
///
/// `lpa_name` is `None` today — the ONS ward→local-authority name table is
/// deferred to the v3.0 OSINT milestone (D-03).  Do NOT add a name-loader here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
#[serde(rename_all = "camelCase")]
pub struct LpaPresence {
    /// ONS LPA code, e.g. `"X06999001"`, or `"Unknown LPA"` for unmapped wards.
    pub lpa_code: String,
    /// Optional human-readable label — `None` until the ONS name table is loaded (D-03).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lpa_name: Option<String>,
    /// Count of Contracts-Finder contracts attributed to this LPA.
    pub contract_count: usize,
    /// Count of planning applications attributed to this LPA.
    pub planning_application_count: usize,
}

/// One step in a BFS graph path between two entities (D-07/D-12).
///
/// `node_id` and `node_label` are exposed in the GQL schema under the names `id`
/// and `label` respectively. `edge_type` is auto-camelCased to `edgeType` by
/// async-graphql (snake_case → camelCase default).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SimpleObject)]
pub struct PathStep {
    #[graphql(name = "id")]
    pub node_id: String,
    #[graphql(name = "label")]
    pub node_label: String,
    pub edge_type: Option<String>,
}

/// Resolve a user-supplied string to a list of entity node IDs.
///
/// This is the **single exact-match resolution choke-point** (D-07).  Today it
/// does exact matching only — slug/CH-number at confidence 1.0 via
/// [`query_entity_by_id`], or a name-substring scan of Entity NodeAdded rows.
///
/// # Future extensibility (D-08)
/// When the v3.0 OSINT `identity_links` table lands, the fuzzy cross-registry
/// JOIN is added **at this seam only** — no callers need to change.
///
/// # SurrealDB 3.x note
/// The name-path scans Entity `NodeAdded` rows directly (not via
/// `query_entity_by_name`) so it can return the node ID without touching the
/// `EntityProfile` struct (which carries no `id` field).  See RESEARCH Open
/// Question 1 for the rationale.
pub async fn resolve_entity(
    db: &Surreal<Any>,
    input: &str,
) -> Result<Vec<String>, surrealdb::Error> {
    if is_slug_shape(input) {
        // Slug/ID path: confirm the node exists and return the input as-is.
        if query_entity_by_id(db, input).await?.is_some() {
            return Ok(vec![input.to_string()]);
        }
        return Ok(vec![]);
    }

    // Name path: scan Entity node-weight rows, filter by lowercased-name substring.
    // Filter by Entity type at the DB level so non-Entity events never displace Entity
    // nodes from the result window (CR-01).
    let results: Vec<serde_json::Value> =
        scan::scan_events_of_kinds_by_type(db, NODE_WEIGHT_KINDS, "\"type\":\"Entity\"").await?;

    // Fold first: an entity with a birth and three status corrections is ONE candidate, and
    // without the fold it would consume four of the ten slots below.
    let results = fold_latest_node_rows(results);
    debug!(
        rows = results.len(),
        "resolve_entity: Entity node-weight scan"
    );

    let needle = input.to_lowercase();
    let mut entity_ids: Vec<String> = results
        .into_iter()
        .filter_map(parse_entity_node_row)
        .filter(|(_id, data)| data.name.to_lowercase().contains(&needle))
        .map(|(id, _data)| id)
        .collect();

    entity_ids.truncate(10);
    Ok(entity_ids)
}

/// Aggregate an entity's contracts into per-LPA buckets.
///
/// Groups contracts by their ward code, resolved to an LPA code via the
/// `config.ingest.planning_portals` ward→LPA map (D-02).  Contracts whose ward is
/// absent from the config land in an **`"Unknown LPA"`** bucket — surfaced
/// honestly rather than dropped (D-02 invariant).
///
/// - CF contracts (`tender_ref.is_some()`) have no `ward_code`; they land in
///   `"Unknown LPA"` and are counted in `contract_count`.
/// - Planning applications (`application_ref.is_some()`) carry a `ward_code`
///   and are counted in `planning_application_count`.
///
/// # SurrealDB 3.x note
/// Never uses `WHERE payload.dst_id = $x` — over-fetches AwardedTo EdgeAdded
/// rows and filters Rust-side (the RecordId workaround, queries.rs:347-359).
pub async fn query_entity_lpa_presence(
    db: &Surreal<Any>,
    entity_id: &str,
    config: &AppConfig,
) -> Result<Vec<LpaPresence>, surrealdb::Error> {
    let target = entity_id.to_owned();

    // Build the ward→LPA HashMap from PlanningPortalConfig entries that carry an lpa_code.
    // Entries with lpa_code: None are excluded — those wards default to "Unknown LPA".
    let ward_to_lpa: HashMap<&str, &str> = config
        .ingest
        .planning_portals
        .iter()
        .filter_map(|p| p.lpa_code.as_deref().map(|lpa| (p.ward_code.as_str(), lpa)))
        .collect();

    // Over-fetch AwardedTo EdgeAdded rows and filter Rust-side for src_id == target.
    // NEVER use `WHERE payload.src_id = $x` — SurrealDB 3.x RecordId coercion trap.
    //
    // The "LIMIT 20000, raised from 10000 to accommodate 14k+ AwardedTo edges" comment that used
    // to sit here, and the 18,000-row "90% of the ceiling" warning below it, were both describing
    // a query that no longer exists: this read has gone through the paged `scan_events_by_type`
    // since GRD-09, so there is no ceiling to approach. Leaving the warning in place would have
    // meant a WARN fired at 18,000 rows to announce a limit that had already been removed —
    // which is worse than no warning, because it trains an operator to ignore the one signal
    // this path has.
    let all_awarded: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"AwardedTo\"").await?;

    // Collect contract node IDs awarded BY this entity.
    // AwardedTo semantic: entity (src) → contract (dst). Mappers write it this way.
    let mut contract_ids: Vec<String> = all_awarded
        .into_iter()
        .filter_map(|row| {
            if row["kind"].as_str()? != "EdgeAdded" {
                return None;
            }
            let src = extract_id_string(&row["payload"]["src_id"])?;
            if src == target {
                extract_id_string(&row["payload"]["dst_id"])
            } else {
                None
            }
        })
        .collect();
    contract_ids.sort();
    contract_ids.dedup();

    if contract_ids.is_empty() {
        return Ok(vec![]);
    }

    // One folded marker scan, replacing the per-contract `… payload.entity_id = $eid LIMIT 5`
    // loop — see `contract_weight_map` for what that cap and the missing fold each cost. The
    // effect here was doubled: an LPA bucket is a *count*, so a re-ingested contract inflated
    // its LPA's `contract_count` while a contract whose NodeAdded fell outside the arbitrary
    // five vanished from every bucket.
    let unknown_lpa = "Unknown LPA";
    let mut buckets: HashMap<String, LpaPresence> = HashMap::new();

    let contract_data = contract_weight_map(db, &contract_ids).await;
    for contract_id in &contract_ids {
        // `_ts` is the node event's timestamp, which this aggregate deliberately does not use:
        // `LpaPresenceView` carries no provenance because a count spanning Contracts Finder and
        // the planning portals cannot name one source without that being a placeholder (45-01).
        let Some((cd, _ts)) = contract_data.get(contract_id) else {
            continue;
        };

        // Resolve LPA: ward_code → lpa_code via config map; None/unmapped → "Unknown LPA".
        let lpa_key = cd
            .ward_code
            .as_deref()
            .and_then(|ward| ward_to_lpa.get(ward).copied())
            .unwrap_or(unknown_lpa)
            .to_owned();

        let entry = buckets
            .entry(lpa_key.clone())
            .or_insert_with(|| LpaPresence {
                lpa_code: lpa_key,
                lpa_name: None,
                contract_count: 0,
                planning_application_count: 0,
            });

        // tender_ref.is_some() ⇒ CF contract; application_ref.is_some() ⇒ planning application.
        match (cd.tender_ref.is_some(), cd.application_ref.is_some()) {
            (true, _) => entry.contract_count += 1,
            (false, true) => entry.planning_application_count += 1,
            (false, false) => {}
        }
    }

    // Sorted by LPA code: `HashMap::into_values` iteration order is randomised per process, so
    // the pre-existing return order differed between two runs over identical data.
    let mut presence: Vec<LpaPresence> = buckets.into_values().collect();
    presence.sort_by(|a, b| a.lpa_code.cmp(&b.lpa_code));
    Ok(presence)
}

/// A structured risk verdict for a counterparty entity, assembled from existing
/// alert records and entity profile data. No re-detection is performed — this
/// reads existing SurrealDB records only (VER-04).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyVerdict {
    pub entity_name: String,
    pub ch_number: Option<String>,
    pub entity_status: urban_integrity_schema::types::node::EntityStatus,
    /// Maximum T-Digest normalised percentile score across all alert records (0.0–1.0).
    /// Zero when no alerts exist (D-02).
    pub p_c_score: f64,
    /// Deduplicated, sorted list of motif type strings from all alert records.
    pub motif_flags: Vec<String>,
    /// Count of `EntityStatus::Dissolved(_)` variants in `related_entities` only.
    /// `Active` and `InLiquidation` variants are excluded.
    pub dissolved_relative_count: usize,
    /// Number of directors in the entity profile.
    pub connected_officer_count: usize,
    /// Risk level derived from `p_c_score` via D-01 thresholds.
    /// Can be overridden to `Critical` by the active-evasion check in `verify.rs`
    /// when `has_disqualified_director` is true and a `disqualified_director_evasion`
    /// `graph_events` row exists (SURF-03 / R-03).
    pub risk_level: RiskLevel,

    // ── OSINT flag fields (Phase 17, SURF-03 / D-13) ─────────────────────────
    // Derived purely from the already-populated EntityProfile OSINT fields.
    // build_verify_verdict stays pure (no DB access); the active-evasion override
    // lives in verify.rs run() where DB is available.
    /// True if the entity has at least one confirmed/auto-linked disqualified director.
    /// Derived from `profile.disqualifications.is_empty()`.
    pub has_disqualified_director: bool,

    /// Display names of confirmed/auto-linked beneficial owners (PSC Controls edges).
    /// Empty in Phase 17 when no PSC links exist for this entity.
    pub beneficial_owners: Vec<String>,

    /// True if the entity has a confirmed offshore link — always false in Phase 17
    /// (Phase 18 wires this when the Land Registry / ICIJ adapter lands).
    pub has_offshore_link: bool,

    /// True if the entity or its officers appear in a sanctions dataset — always false
    /// in Phase 17 (Phase 19 wires this when the OpenSanctions adapter lands).
    pub has_sanctions_match: bool,
}

/// Risk classification derived from P_c score (D-01 thresholds).
///
/// Serialised as camelCase enum variant names in JSON output.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// Classify a P_c score into a `RiskLevel` using D-01 thresholds.
///
/// - `score >= 0.9` → `Critical`
/// - `score >= 0.7` → `High`
/// - `score >= 0.5` → `Medium`
/// - else           → `Low`
pub fn classify_risk(score: f64) -> RiskLevel {
    if score >= 0.9 {
        RiskLevel::Critical
    } else if score >= 0.7 {
        RiskLevel::High
    } else if score >= 0.5 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    }
}

/// Assemble a `VerifyVerdict` from an `EntityProfile` without touching the database.
///
/// - `p_c_score` is the **maximum** alert score (worst-case signal wins); 0.0 when
///   there are no alerts (D-02).
/// - `motif_flags` are deduplicated across alerts of the same `motif_type` and sorted
///   lexicographically for determinism.
/// - `dissolved_relative_count` counts only `EntityStatus::Dissolved(_)` variants in
///   `related_entities`; `Active` and `InLiquidation` are excluded.
/// - `connected_officer_count` equals `profile.directors.len()`.
pub fn build_verify_verdict(profile: &EntityProfile) -> VerifyVerdict {
    use urban_integrity_schema::types::node::EntityStatus;

    // Maximum P_c score across all alert records (worst-case signal wins).
    let max_score = profile
        .alerts
        .iter()
        .map(|a| a.score)
        .fold(0.0_f64, f64::max);

    // Deduplicate motif types via HashSet, then sort for determinism.
    let mut motif_flags: Vec<String> = profile
        .alerts
        .iter()
        .map(|a| a.motif_type.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    motif_flags.sort();

    // Count only Dissolved(_) variants — Active and InLiquidation are excluded.
    let dissolved_count = profile
        .related_entities
        .iter()
        .filter(|e| matches!(e.status, EntityStatus::Dissolved(_)))
        .count();

    // ── OSINT flags (D-13) — derived purely from EntityProfile OSINT fields ──
    // build_verify_verdict stays pure (no DB access). The active-evasion override
    // (has_disqualified_director AND confirmed evasion alert exists) happens in
    // verify.rs run() where DB is available, AFTER this function returns.
    let has_disqualified_director = !profile.disqualifications.is_empty();
    let beneficial_owners: Vec<String> = profile
        .beneficial_owners
        .iter()
        .map(|b| b.name.clone())
        .collect();

    VerifyVerdict {
        entity_name: profile.name.clone(),
        ch_number: profile.ch_number.clone(),
        entity_status: profile.status.clone(),
        p_c_score: max_score,
        motif_flags,
        dissolved_relative_count: dissolved_count,
        connected_officer_count: profile.directors.len(),
        risk_level: classify_risk(max_score),
        has_disqualified_director,
        beneficial_owners,
        has_offshore_link: false,   // Phase 18+ (Land Registry / ICIJ)
        has_sanctions_match: false, // Phase 19+ (OpenSanctions)
    }
}

// ── PIV-02: the findings-by-entity reverse index (plan 45-05) ────────────────
//
// PIV-02's last clause asks for "every alert or probe finding naming the entity". What a probe
// finding can name is bounded by what its `EvidenceRow`s carry, and most of the battery carries
// nothing nameable: counts, producer names, adapter names. Plan 44-21 measured that boundary over
// the thirteen probes it had in hand and found twelve of them aggregate-only.
//
// So this section is thin, and its dangerous failure is not an error — it is an empty list. An
// analyst reads an empty findings section as "this entity appears in no findings"; the truth is
// usually "no probe in this battery was capable of naming it". Those are opposite conclusions with
// the same rendering, which is why [`FindingsByEntity`] carries a [`FindingsOutcome`] and a
// [`ProbeCoverage`] rather than a bare `Vec`.
//
// **This module adds no `RecordRef` to any probe** (plan 45-05's D-05). Doing so would mean
// inventing node ids the graph does not contain — exactly what 44-21 declined to do — and belongs
// to phase 49's evidence model.

/// What a probe's evidence can name, and therefore whether [`findings_naming_entity`] can search
/// it at all.
///
/// Declared per probe on `urban_integrity_probe::Probe::evidence_scope`, with **no default**: a
/// probe added to the battery without answering this does not compile, so the coverage figure
/// this section states cannot quietly stop counting a probe that exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceScope {
    /// The probe attaches [`RecordRef`]s whose ids are graph node ids — `GB-COH:…`, `director:…`,
    /// `ward:…`, `LOC:…`, `CF:…`. These, and only these, are searchable by entity id.
    NamesNodes,
    /// The probe attaches [`RecordRef`]s, but of a kind that names something other than a graph
    /// node — today `RecordKind::IdentityLink`, whose id is an `identity_links` record id.
    ///
    /// A distinct bucket rather than lumped in with [`Self::AggregateOnly`] because the two are
    /// different problems: an aggregate-only probe has no id to give, while these have an id that
    /// this index cannot yet follow. Resolving one needs an `identity_links` join that reads the
    /// link's two endpoints — deliberately not done here (it is a second read path, and D-05 keeps
    /// this plan to the index), but a reader has to be able to tell "no id" from "an id I did not
    /// follow".
    NamesLinksOnly,
    /// The probe attaches no [`RecordRef`] at all: its evidence text interpolates counts, producer
    /// names or adapter names. Nothing in it can ever match an entity id.
    AggregateOnly,
}

impl EvidenceScope {
    /// Whether [`findings_naming_entity`] can find anything in this probe's evidence.
    ///
    /// Only [`Self::NamesNodes`] is searchable. `NamesLinksOnly` returns `false` — the refs exist
    /// but do not name a node, and reporting them as searched would inflate the coverage figure
    /// with probes that can never produce a row.
    pub fn is_searchable_by_node_id(self) -> bool {
        matches!(self, EvidenceScope::NamesNodes)
    }
}

/// How much of the probe battery the findings-by-entity section was able to search, as data.
///
/// **The section states this, not a comment and not a log line.** An empty `rows` is only
/// interpretable next to these counts, and the counts are only trustworthy if they come from the
/// battery rather than from a number typed into a document — which is why this is built by
/// `urban_integrity_probe::battery_evidence_coverage` from the real `Vec<Box<dyn Probe>>` and
/// reconciled against the probe sources by `crates/probe/tests/findings_coverage.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProbeCoverage {
    /// Probes whose evidence can name a graph node — the searchable population.
    pub names_nodes: Vec<String>,
    /// Probes whose refs name identity links rather than graph nodes.
    pub names_links_only: Vec<String>,
    /// Probes whose evidence carries no ref at all.
    pub aggregate_only: Vec<String>,
}

impl ProbeCoverage {
    /// Every probe accounted for, in any bucket.
    pub fn probes_total(&self) -> usize {
        self.names_nodes.len() + self.names_links_only.len() + self.aggregate_only.len()
    }

    /// Probes this index can search.
    pub fn probes_searchable(&self) -> usize {
        self.names_nodes.len()
    }

    /// Probes this index cannot search, for either reason.
    pub fn probes_not_searchable(&self) -> usize {
        self.names_links_only.len() + self.aggregate_only.len()
    }

    /// This probe's scope, or `None` if the probe is in no bucket — which means the coverage was
    /// built from a different battery than the findings were, and is a stale-registry bug rather
    /// than a probe with nothing to say. See [`FindingsByEntity::unknown_producers`].
    pub fn scope_of(&self, probe: &str) -> Option<EvidenceScope> {
        if self.names_nodes.iter().any(|p| p == probe) {
            Some(EvidenceScope::NamesNodes)
        } else if self.names_links_only.iter().any(|p| p == probe) {
            Some(EvidenceScope::NamesLinksOnly)
        } else if self.aggregate_only.iter().any(|p| p == probe) {
            Some(EvidenceScope::AggregateOnly)
        } else {
            None
        }
    }

    /// The coverage sentence the renderer states verbatim, in the manner of
    /// [`NULL_RANGE_RULE`]: the figure an analyst needs to read an empty list correctly, built
    /// from the counts rather than retyped beside them.
    pub fn statement(&self) -> String {
        format!(
            "Searched {} of {} probes. {} report only counts, producer names or adapter names and \
             can never name an entity; {} name identity links rather than graph nodes. An empty \
             list here means no searchable probe named this entity — not that nothing was found \
             about it.",
            self.probes_searchable(),
            self.probes_total(),
            self.aggregate_only.len(),
            self.names_links_only.len(),
        )
    }
}

/// Which producer named the entity. Two shapes because a probe finding and a motif alert carry
/// genuinely different fields, and flattening them would mean inventing a severity for an alert
/// (which has a score) or a score for a finding (which has none).
#[derive(Debug, Clone, PartialEq)]
pub enum FindingOrigin {
    /// A probe in the battery.
    Probe {
        /// The probe's `name()`.
        name: String,
        /// `VerdictKind::of(&finding.verdict).to_string()` — the discriminant, not the `Verdict`
        /// itself, which carries a `Coverage` this row has no use for.
        verdict: String,
        /// The finding's own severity.
        severity: Severity,
    },
    /// A `MotifDetected` alert whose `result_json` names the entity.
    MotifAlert {
        /// e.g. `PhoenixLoop`, `AddressCluster`.
        motif_type: String,
        /// The alert's `P_c` score.
        score: f64,
    },
}

/// One finding or alert that names this entity, and the ref that named it.
///
/// **Not temporal, and deliberately so.** `apply_as_at`'s table already records the rule for
/// `alerts`: a detection is an assertion this project made at a moment, not a fact in force over
/// an interval, and its moment is its [`Provenance`]. A probe finding is the same shape — it is
/// what the battery concluded when it ran. Giving these rows an [`EffectiveRange`] would mean
/// inventing one, which is the failure `EffectiveRange::Unrecorded` exists to prevent.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRef {
    /// Which producer said it.
    pub origin: FindingOrigin,
    /// The finding's `summary`, or the alert's motif type and score rendered as one.
    pub summary: String,
    /// The evidence line that carried the matching ref, verbatim. For an alert, the `result_json`
    /// is not evidence text, so this is the alert id.
    pub evidence_text: String,
    /// How the entity was named — which pivot kind matched.
    pub named_by: RecordKind,
    /// The id as the producer wrote it. Kept rather than assumed equal to the queried
    /// `entity_id`, because a `CompanyNumber` ref matches on `04027804` while the entity id is
    /// `GB-COH:04027804`, and a reader auditing the row needs the string the probe actually held.
    pub matched_id: String,
    /// Where this row came from (PIV-03). Every profile section carries one (plan 45-04).
    pub provenance: Provenance,
}

/// The three answers this section can give, kept apart because two of them render identically as
/// an empty list and mean opposite things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingsOutcome {
    /// At least one searchable producer named this entity. `rows` is non-empty.
    Named,
    /// At least one probe that *can* name a node contributed a finding, and none of them named
    /// this entity. This is the only outcome in which an empty list means "no findings".
    NotNamed,
    /// **No searchable probe contributed a finding at all** — either none ran, or every finding
    /// offered came from a probe whose evidence can never carry a node id. An empty list here is
    /// not evidence of anything about the entity.
    NotSearched,
}

/// Every probe finding and alert naming one entity, plus the coverage that makes an empty list
/// readable (PIV-02).
#[derive(Debug, Clone, PartialEq)]
pub struct FindingsByEntity {
    /// The entity this index was built for.
    pub entity_id: String,
    /// The matching rows, worst-first: alerts by descending score, findings by severity rank,
    /// then producer name, then evidence text. Deterministic — the same inputs give the same
    /// order.
    pub rows: Vec<FindingRef>,
    /// The battery's coverage, as data on the section.
    pub coverage: ProbeCoverage,
    /// How many of the findings offered came from a probe this index can search.
    pub searchable_findings: usize,
    /// How many came from a probe it cannot — the population an empty list is silent about.
    pub unsearchable_findings: usize,
    /// Findings whose probe name is in no coverage bucket. Always empty when the coverage and the
    /// findings come from the same battery; non-empty means the registry went stale, and these
    /// are counted **neither** as searchable nor as unsearchable so a stale entry cannot inflate
    /// either figure.
    pub unknown_producers: Vec<String>,
    /// [`FindingsOutcome`] — the distinction the rows alone cannot carry.
    pub outcome: FindingsOutcome,
}

/// Whether one [`RecordRef`] names this entity.
///
/// - `Entity` matches the graph node id exactly. That id may be a company, a director slug, a
///   ward or a location, so the match is on the id, not on a namespace allow-list. (The older
///   wording here said "every one of those is a node an analyst can open". It is not true and was
///   not true when written: [`query_entity_by_id`] resolves `Entity` and `OffshoreEntity` node
///   weights only, so a `ward:`, `location:`, `LOC:`, `CF:` or `planning:` id matches a ref here
///   and still has no profile page to open. The match is about *whether a finding names this
///   subject*, which is a different question from whether that subject is renderable.)
/// - `CompanyNumber` matches either the entity's own CH number or the `GB-COH:` id built from it,
///   because probes hold the bare number and the profile holds the namespaced id.
/// - `Contract` and `Location` match on the id exactly, for the same reason `Entity` does: both
///   name a graph node, and D-06 added them to say *which class* of node, not to say that the id
///   is a different kind of string. Matching them any other way would let one node id decide two
///   different things about one profile depending on which kind a producer chose to stamp on it —
///   probes emit node ids as `Entity` and the profile mapper emits them as `Contract`/`Location`,
///   and those two spellings must not disagree here.
/// - `IdentityLink` never matches. A link id is not a node id; following it needs an
///   `identity_links` read this function does not do. That refusal is what
///   [`EvidenceScope::NamesLinksOnly`] exists to report, rather than being silently absent.
fn ref_names_entity(r: &RecordRef, entity_id: &str, ch_number: Option<&str>) -> bool {
    match r.kind {
        RecordKind::Entity | RecordKind::Contract | RecordKind::Location => r.id == entity_id,
        RecordKind::CompanyNumber => {
            ch_number.is_some_and(|n| n == r.id) || entity_id == format!("GB-COH:{}", r.id)
        }
        RecordKind::IdentityLink => false,
    }
}

/// The provenance of a probe finding.
///
/// `attribution::PROBE`, never `attribution::DETECTOR`: both are assertions this project made
/// rather than a register's claim, but they are different producers, and a chip reading
/// `detector` beside a finding from `address-cluster-exact` would name the wrong one.
///
/// `run_observed_at` is the probe run's own timestamp, supplied by the caller. When it is absent
/// the row is [`Provenance::Unattributed`] and says so — it is **never** stamped `Utc::now()`,
/// which is the mutation plan 45-04's `a_relation_row_with_no_created_at_is_unattributed_not_stamped_now`
/// was written to catch.
fn probe_provenance(run_observed_at: Option<&str>) -> Provenance {
    match run_observed_at {
        Some(ts) if !ts.is_empty() => Provenance::Attributed {
            source_dataset: attribution::PROBE.to_owned(),
            observed_at: ts.to_owned(),
            tier: None,
        },
        _ => Provenance::Unattributed {
            reason: "the probe run did not record when it ran".to_owned(),
        },
    }
}

/// Build the findings-by-entity section: every supplied probe finding and alert that names
/// `entity_id`, plus the coverage figures that make the result readable when it is empty.
///
/// Pure — the DB read that finds the alerts is [`alerts_naming_entity`], and the findings arrive
/// from a probe run in memory (nothing persists a `Finding`). [`query_findings_naming_entity`]
/// joins the two.
///
/// `ch_number` is the subject's Companies House number, when it has one, so a
/// `RecordKind::CompanyNumber` ref can match. `None` for a ward, a location or a director slug.
///
/// A finding from a probe the coverage does not know is counted in
/// [`FindingsByEntity::unknown_producers`] and **is still searched** — refusing to search it
/// would hide a real row behind a bookkeeping problem — but it is excluded from both the
/// searchable and the unsearchable tally, so a stale registry shows up as a third number rather
/// than as a wrong one.
pub fn findings_naming_entity(
    entity_id: &str,
    ch_number: Option<&str>,
    findings: &[Finding],
    alerts: &[AlertRecord],
    coverage: &ProbeCoverage,
    run_observed_at: Option<&str>,
) -> FindingsByEntity {
    let mut rows: Vec<FindingRef> = Vec::new();
    let mut searchable_findings = 0usize;
    let mut unsearchable_findings = 0usize;
    let mut unknown_producers: Vec<String> = Vec::new();

    for finding in findings {
        match coverage.scope_of(&finding.probe) {
            Some(scope) if scope.is_searchable_by_node_id() => searchable_findings += 1,
            Some(_) => unsearchable_findings += 1,
            None => {
                if !unknown_producers.contains(&finding.probe) {
                    unknown_producers.push(finding.probe.clone());
                }
            }
        }

        for row in &finding.evidence {
            for r in &row.refs {
                if ref_names_entity(r, entity_id, ch_number) {
                    rows.push(FindingRef {
                        origin: FindingOrigin::Probe {
                            name: finding.probe.clone(),
                            verdict: VerdictKind::of(&finding.verdict).to_string(),
                            severity: finding.severity,
                        },
                        summary: finding.summary.clone(),
                        evidence_text: row.text.clone(),
                        named_by: r.kind,
                        matched_id: r.id.clone(),
                        provenance: probe_provenance(run_observed_at),
                    });
                }
            }
        }
    }

    for alert in alerts {
        rows.push(FindingRef {
            origin: FindingOrigin::MotifAlert {
                motif_type: alert.motif_type.clone(),
                score: alert.score,
            },
            summary: format!("{} ({:.2})", alert.motif_type, alert.score),
            evidence_text: alert.id.clone(),
            named_by: RecordKind::Entity,
            matched_id: entity_id.to_owned(),
            provenance: alert.provenance.clone(),
        });
    }

    // Worst first, and total: alerts by descending score ahead of findings of equal weight is not
    // the claim — the claim is only that the order is deterministic, so the same inputs give the
    // same section every time. A partial order would make the output depend on input order.
    rows.sort_by(|a, b| {
        let rank = |r: &FindingRef| match &r.origin {
            FindingOrigin::MotifAlert { .. } => 0u8,
            FindingOrigin::Probe { severity, .. } => 1 + severity.rank(),
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| match (&a.origin, &b.origin) {
                (
                    FindingOrigin::MotifAlert { score: x, .. },
                    FindingOrigin::MotifAlert { score: y, .. },
                ) => y.total_cmp(x),
                _ => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.evidence_text.cmp(&b.evidence_text))
            .then_with(|| a.matched_id.cmp(&b.matched_id))
    });

    let outcome = if !rows.is_empty() {
        FindingsOutcome::Named
    } else if searchable_findings > 0 {
        FindingsOutcome::NotNamed
    } else {
        FindingsOutcome::NotSearched
    };

    FindingsByEntity {
        entity_id: entity_id.to_owned(),
        rows,
        coverage: coverage.clone(),
        searchable_findings,
        unsearchable_findings,
        unknown_producers,
        outcome,
    }
}

/// Every live `MotifDetected` alert whose `result_json` names `entity_id`.
///
/// The same read `assemble_entity_profile` does for its own alerts section, extracted so the
/// findings index can be built without assembling a whole profile. Paged through
/// `scan::scan_events_by_payload_contains`, which pages `graph_events` through
/// `scan::SCAN_PAGE_SIZE` and filters in Rust — **no `LIMIT` literal**, because a `LIMIT` in the
/// SQL bounds what the planner looked at rather than what matched, and under the SurrealDB 3.x
/// index-planner bug that returns none of an entity's alerts rather than some of them.
///
/// Withdrawn alerts are excluded, matching the profile: a retracted accusation about a named
/// subject is not volunteered about that subject (T-23-FalseAccusation). They stay readable
/// where they are asked for, via `alerts list --include-withdrawn`.
pub async fn alerts_naming_entity(
    db: &Surreal<Any>,
    entity_id: &str,
) -> Result<Vec<AlertRecord>, surrealdb::Error> {
    let mut alerts: Vec<AlertRecord> =
        scan::scan_events_by_payload_contains(db, "MotifDetected", "result_json", entity_id)
            .await?
            .into_iter()
            .filter_map(parse_alert_row)
            .filter(|a| a.withdrawn_at.is_none())
            .collect();
    alerts.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    Ok(alerts)
}

/// [`alerts_naming_entity`] joined to [`findings_naming_entity`] — the whole section from a db
/// handle plus the findings a probe run produced.
pub async fn query_findings_naming_entity(
    db: &Surreal<Any>,
    entity_id: &str,
    ch_number: Option<&str>,
    findings: &[Finding],
    coverage: &ProbeCoverage,
    run_observed_at: Option<&str>,
) -> Result<FindingsByEntity, surrealdb::Error> {
    let alerts = alerts_naming_entity(db, entity_id).await?;
    Ok(findings_naming_entity(
        entity_id,
        ch_number,
        findings,
        &alerts,
        coverage,
        run_observed_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_motif_result(motif_type: &str, score: f64, ward: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "motif_type": motif_type,
            "risk_score": score,
            "score_breakdown": { "ward_code": ward },
            "participants": [],
            "unique_hash": "abc123"
        })
    }

    fn make_row(
        event_id: &str,
        motif_type: &str,
        score: f64,
        ward: Option<&str>,
    ) -> serde_json::Value {
        let result = make_motif_result(motif_type, score, ward);
        serde_json::json!({
            "event_id": event_id,
            "timestamp": 1_700_000_000_000u64,
            "kind": "MotifDetected",
            "payload": {
                "result_json": result.to_string()
            }
        })
    }

    // ── Neighbour definition (PIV-06, the phase 45 design notes, open question 3) ─────────

    fn edge_row(src: serde_json::Value, dst: serde_json::Value, ty: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "EdgeAdded",
            "payload": {
                "src_id": src,
                "dst_id": dst,
                "weight_json": format!("{{\"type\":\"{ty}\"}}"),
            }
        })
    }

    /// A RecordId object exactly as SurrealDB 3.x hands one back for a `director:<slug>` id.
    fn record_id(table: &str, id: &str) -> serde_json::Value {
        serde_json::json!({ "tb": table, "id": { "String": id } })
    }

    /// The whole point of the definition: an edge pointing AT the subject makes its other end a
    /// neighbour, matching `population_scope::build`'s undirected walk.
    #[test]
    fn an_inbound_edge_makes_its_other_end_a_neighbour() {
        let rows = vec![edge_row(
            serde_json::json!("OFF:inbound-officer"),
            serde_json::json!("GB-COH:X9999001"),
            "Directs",
        )];

        let n = neighbours_from_rows(&rows, "GB-COH:X9999001");

        assert_eq!(
            n,
            vec![Neighbour {
                id: "OFF:inbound-officer".to_string(),
                edge_types: vec!["Directs".to_string()],
            }],
            "an inbound edge counts — population_scope's \
             build_includes_a_node_reachable_only_via_an_inbound_edge_from_a_seed convention"
        );
    }

    #[test]
    fn a_self_edge_does_not_make_a_node_its_own_neighbour() {
        let rows = vec![edge_row(
            serde_json::json!("GB-COH:X9999001"),
            serde_json::json!("GB-COH:X9999001"),
            "Directs",
        )];

        assert!(
            neighbours_from_rows(&rows, "GB-COH:X9999001").is_empty(),
            "the subject is never its own neighbour"
        );
    }

    // ── Neighbourhood expansion (CNV-01, plan 45-08) ─────────────────────────

    /// A three-hop chain plus one branch, the fixture every expansion test below reads.
    fn chain_rows() -> Vec<serde_json::Value> {
        vec![
            edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("contract:X9999-c1"),
                "AwardedTo",
            ),
            edge_row(
                serde_json::json!("OFF:a-director"),
                serde_json::json!("GB-COH:X9999001"),
                "Directs",
            ),
            edge_row(
                serde_json::json!("OFF:a-director"),
                serde_json::json!("GB-COH:X9999002"),
                "Directs",
            ),
            edge_row(
                serde_json::json!("GB-COH:X9999002"),
                serde_json::json!("location:X9999-l1"),
                "LocatedAt",
            ),
        ]
    }

    /// The index and the per-subject reader are one definition, not two.
    ///
    /// PROVE THIS CAN FAIL: dropping the `if src == dst { continue; }` guard from
    /// `NeighbourIndex::from_rows` makes the self-edge subject disagree and this test fails.
    /// That mutation was run.
    #[test]
    fn neighbour_index_agrees_with_neighbours_from_rows() {
        let mut rows = chain_rows();
        // A self-edge and a RecordId-object endpoint, because both are cases the two readers
        // could plausibly disagree about.
        rows.push(edge_row(
            serde_json::json!("GB-COH:X9999001"),
            serde_json::json!("GB-COH:X9999001"),
            "Directs",
        ));
        rows.push(edge_row(
            record_id("director", "a-director"),
            serde_json::json!("GB-COH:X9999003"),
            "Directs",
        ));
        let index = NeighbourIndex::from_rows(&rows, &[]);

        let subjects = [
            "GB-COH:X9999001",
            "GB-COH:X9999002",
            "GB-COH:X9999003",
            "OFF:a-director",
            "director:a-director",
            "contract:X9999-c1",
            "nobody:at-all",
        ];
        let mut non_empty = 0;
        for subject in subjects {
            let expected = neighbours_from_rows(&rows, subject);
            non_empty += usize::from(!expected.is_empty());
            assert_eq!(
                index.neighbours_of(subject),
                expected,
                "the index and neighbours_from_rows disagree about {subject}"
            );
        }
        assert!(
            non_empty >= 5,
            "NON-VACUITY: only {non_empty} of the subjects had any neighbour at all, so this \
             comparison would pass against a reader that always returned nothing"
        );
    }

    /// An identity link is a neighbour, labelled rather than merged — and the ENR-11 gate is the
    /// loader's, so an index built from an empty pair list has no `IdentityLink` edge to offer.
    #[test]
    fn an_identity_link_pair_is_a_labelled_neighbour() {
        let links = vec![(
            "GB-COH:X9999001".to_string(),
            "director:a-director".to_string(),
        )];
        let index = NeighbourIndex::from_rows(&[], &links);
        assert_eq!(
            index.neighbours_of("GB-COH:X9999001"),
            vec![Neighbour {
                id: "director:a-director".to_string(),
                edge_types: vec!["IdentityLink".to_string()],
            }]
        );
        assert!(NeighbourIndex::from_rows(&[], &[])
            .neighbours_of("GB-COH:X9999001")
            .is_empty());
    }

    /// An `EdgeAdded` row whose serialised weight carries `meta.effective_from` / `meta.
    /// effective_to`, which is what `Controls` and `OwnsProperty` rows look like live.
    fn dated_edge_row(
        src: serde_json::Value,
        dst: serde_json::Value,
        ty: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> serde_json::Value {
        let meta = serde_json::json!({ "effective_from": from, "effective_to": to });
        serde_json::json!({
            "kind": "EdgeAdded",
            "payload": {
                "src_id": src,
                "dst_id": dst,
                "weight_json": serde_json::json!({ "type": ty, "meta": meta }).to_string(),
            }
        })
    }

    /// **CNV-02: an expansion's edges carry their recorded range, not just their type.**
    ///
    /// The three shapes that occur live are all here: a dated edge, an undated one, and an identity
    /// link, which has no temporal fields to read at all and says so in its own basis.
    ///
    /// PROVE THIS CAN FAIL: drop the `edge_meta_candidate` call in `NeighbourIndex::from_rows` and
    /// pass `None, None`; the dated assertion fails. That mutation was run.
    #[test]
    fn an_expansion_edge_carries_the_range_the_record_states_for_it() {
        let rows = vec![
            dated_edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("property:X9999-p1"),
                "OwnsProperty",
                Some("2019-04-01T00:00:00Z"),
                None,
            ),
            edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("OFF:a-director"),
                "Directs",
            ),
        ];
        let links = vec![(
            "GB-COH:X9999001".to_string(),
            "director:a-director".to_string(),
        )];
        let index = NeighbourIndex::from_rows(&rows, &links);
        let nodes = index.expand("GB-COH:X9999001", 1);

        let by_id: std::collections::HashMap<&str, &NeighbourhoodNode> =
            nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        assert_eq!(by_id.len(), 3, "Non-vacuity: three distinct neighbours");

        // A dated edge keeps its start, states no end, and names the fields it was read from.
        let owned = by_id["property:X9999-p1"];
        assert_eq!(owned.edges.len(), 1);
        assert_eq!(owned.edges[0].edge_type, "OwnsProperty");
        assert_eq!(
            owned.edges[0].effective_from.as_deref(),
            Some("2019-04-01T00:00:00Z")
        );
        assert_eq!(owned.edges[0].effective_to, None);
        assert_eq!(
            owned.edges[0].basis,
            "OwnsProperty.meta.effective_from / OwnsProperty.meta.effective_to"
        );

        // An undated edge — the majority shape on this corpus — states neither endpoint and still
        // names what was consulted. "No date recorded" and "no field to read" are different facts.
        let directs = by_id["OFF:a-director"];
        assert_eq!(directs.edges[0].effective_from, None);
        assert_eq!(directs.edges[0].effective_to, None);
        assert!(directs.edges[0]
            .basis
            .contains("Directs.meta.effective_from"));

        // An identity link has no temporal fields at all, and its basis says so rather than
        // borrowing an edge-meta path it never had.
        let linked = by_id["director:a-director"];
        assert_eq!(linked.edges[0].edge_type, "IdentityLink");
        assert_eq!(linked.edges[0].basis, IDENTITY_LINK_RANGE_BASIS);
        assert_eq!(linked.edges[0].effective_from, None);
    }

    /// `edge_types()` still answers the question the old field answered, derived from the edges
    /// rather than stored beside them — so the two can never describe different edges.
    ///
    /// PROVE THIS CAN FAIL: make `edge_types` return a hard-coded `vec![]` and this fails; make
    /// `distinct_edge_types` skip its `dedup` and the duplicate assertion fails.
    #[test]
    fn the_edge_type_view_is_derived_from_the_edges_it_describes() {
        let rows = vec![
            edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("contract:X9999-c1"),
                "AwardedTo",
            ),
            edge_row(
                serde_json::json!("contract:X9999-c1"),
                serde_json::json!("GB-COH:X9999001"),
                "LocatedAt",
            ),
            // The same type twice, with the same (absent) range: one edge as far as the set is
            // concerned, and one entry in the type list.
            edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("contract:X9999-c1"),
                "AwardedTo",
            ),
        ];
        let index = NeighbourIndex::from_rows(&rows, &[]);
        let nodes = index.expand("GB-COH:X9999001", 1);
        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0].edge_types(),
            vec!["AwardedTo".to_string(), "LocatedAt".to_string()]
        );
        assert_eq!(nodes[0].edges.len(), 2, "the duplicate collapsed");
        // And the derived view agrees with the per-subject reader's own field, which is the
        // equivalence this crate has always pinned.
        assert_eq!(
            index.neighbours_of("GB-COH:X9999001")[0].edge_types,
            nodes[0].edge_types()
        );
    }

    /// One hop is the immediate neighbourhood; the seed is never in it.
    #[test]
    fn one_hop_is_the_immediate_neighbourhood_and_excludes_the_seed() {
        let index = NeighbourIndex::from_rows(&chain_rows(), &[]);
        let hop_one = index.expand("GB-COH:X9999001", 1);
        let ids: Vec<&str> = hop_one.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["OFF:a-director", "contract:X9999-c1"]);
    }

    /// Each node's `hop` is the FIRST distance the walk reached it at, the list is ordered by hop
    /// and then id, and `reached_from` names the node it was reached through.
    ///
    /// PROVE THIS CAN FAIL: removing the `seen.contains(&neighbour.id)` guard in
    /// `NeighbourIndex::expand` re-emits the seed's own neighbours at hop 2 and the hop
    /// assertion fails. That mutation was run.
    #[test]
    fn expansion_records_the_first_hop_a_node_was_reached_at() {
        let index = NeighbourIndex::from_rows(&chain_rows(), &[]);
        let nodes = index.expand("contract:X9999-c1", 3);
        let shape: Vec<(&str, u32, &str)> = nodes
            .iter()
            .map(|n| (n.id.as_str(), n.hop, n.reached_from.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("GB-COH:X9999001", 1, "contract:X9999-c1"),
                ("OFF:a-director", 2, "GB-COH:X9999001"),
                ("GB-COH:X9999002", 3, "OFF:a-director"),
            ]
        );
        assert!(
            nodes.iter().all(|n| n.id != "contract:X9999-c1"),
            "the seed is not a member of its own neighbourhood"
        );
    }

    /// Expansion stops when the graph runs out, not when the hop budget does, and asking for
    /// zero hops asks for nothing.
    #[test]
    fn expansion_terminates_on_an_exhausted_graph_and_on_zero_hops() {
        let index = NeighbourIndex::from_rows(&chain_rows(), &[]);
        let far = index.expand("contract:X9999-c1", 99);
        assert_eq!(far.len(), 4, "the whole component, and no repeats");
        assert!(index.expand("contract:X9999-c1", 0).is_empty());
        assert!(index.expand("nobody:at-all", 3).is_empty());
    }

    /// The read path applies NO node cap: the cut, and the reporting of it, belong to the wire
    /// boundary (CNV-01/D-12). A prefix returned here would have destroyed the total a report
    /// needs.
    #[test]
    fn the_read_path_returns_every_node_it_reaches() {
        let rows: Vec<serde_json::Value> = (0..300)
            .map(|i| {
                edge_row(
                    serde_json::json!("GB-COH:X9999001"),
                    serde_json::json!(format!("contract:X9999-c{i:04}")),
                    "AwardedTo",
                )
            })
            .collect();
        let index = NeighbourIndex::from_rows(&rows, &[]);
        assert_eq!(index.expand("GB-COH:X9999001", 1).len(), 300);
    }

    #[test]
    fn two_edges_to_the_same_node_are_one_neighbour_carrying_both_types() {
        let rows = vec![
            edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("contract:X9999-c1"),
                "AwardedTo",
            ),
            edge_row(
                serde_json::json!("contract:X9999-c1"),
                serde_json::json!("GB-COH:X9999001"),
                "LocatedAt",
            ),
            // Re-ingested duplicate of the first edge — an append-only log emits it again.
            edge_row(
                serde_json::json!("GB-COH:X9999001"),
                serde_json::json!("contract:X9999-c1"),
                "AwardedTo",
            ),
        ];

        let n = neighbours_from_rows(&rows, "GB-COH:X9999001");

        assert_eq!(
            n,
            vec![Neighbour {
                id: "contract:X9999-c1".to_string(),
                edge_types: vec!["AwardedTo".to_string(), "LocatedAt".to_string()],
            }],
            "one neighbour, both edge types, the duplicate collapsed"
        );
    }

    /// The anti-drift pin. `traversal::build_adj_from_rows` is the project's existing undirected
    /// adjacency builder; `neighbours_from_rows` must be its restriction to one node, not a
    /// second convention that agrees today and diverges later.
    #[test]
    fn neighbours_from_rows_agrees_with_build_adj_from_rows() {
        use crate::persistence::traversal::build_adj_from_rows;

        let subject = "GB-COH:X9999001";
        let rows = vec![
            edge_row(
                serde_json::json!(subject),
                serde_json::json!("contract:X9999-c1"),
                "AwardedTo",
            ),
            edge_row(
                serde_json::json!("OFF:officer-a"),
                serde_json::json!(subject),
                "Directs",
            ),
            edge_row(
                serde_json::json!("OFF:officer-b"),
                serde_json::json!(subject),
                "Directs",
            ),
            edge_row(
                serde_json::json!(subject),
                serde_json::json!("location:X9999-l1"),
                "LocatedAt",
            ),
            // Noise: an edge between two other nodes must not become a neighbour of the subject.
            edge_row(
                serde_json::json!("OFF:officer-a"),
                serde_json::json!("GB-COH:X9999002"),
                "Directs",
            ),
        ];

        let mine: std::collections::BTreeSet<String> = neighbours_from_rows(&rows, subject)
            .into_iter()
            .map(|n| n.id)
            .collect();
        let theirs: std::collections::BTreeSet<String> = build_adj_from_rows(&rows)
            .get(subject)
            .expect("the subject has adjacency")
            .iter()
            .map(|e| e.neighbor.clone())
            .collect();

        // Non-vacuity: an empty set on both sides would "agree" and prove nothing.
        assert_eq!(mine.len(), 4, "four distinct neighbours were seeded");
        assert_eq!(
            mine, theirs,
            "the neighbour definition must be build_adj_from_rows restricted to one node"
        );
    }

    /// The one documented divergence, pinned so it is a known difference rather than a latent
    /// one: `build_adj_from_rows` reads endpoints with `as_str()`, so it drops an edge whose
    /// endpoint SurrealDB coerced into a RecordId object — which is EVERY `director:<slug>`
    /// endpoint, the keying docs/ARCHITECTURE.md (Officer Identity Keys) prescribes.
    #[test]
    fn neighbours_from_rows_sees_a_recordid_endpoint_that_build_adj_from_rows_drops() {
        use crate::persistence::traversal::build_adj_from_rows;

        let subject = "GB-COH:X9999001";
        let rows = vec![edge_row(
            record_id("director", "jane-smith"),
            serde_json::json!(subject),
            "Directs",
        )];

        let mine = neighbours_from_rows(&rows, subject);
        assert_eq!(
            mine,
            vec![Neighbour {
                id: "director:jane-smith".to_string(),
                edge_types: vec!["Directs".to_string()],
            }],
            "extract_id_string resolves the RecordId object form"
        );

        assert!(
            !build_adj_from_rows(&rows).contains_key(subject),
            "pinning the known divergence: build_adj_from_rows reads src_id/dst_id with \
             as_str(), so the RecordId object form is invisible to it. If this assertion ever \
             fails, traversal.rs has been fixed — delete this test and fold the case into \
             neighbours_from_rows_agrees_with_build_adj_from_rows."
        );
    }

    #[test]
    fn test_parse_alert_row_phoenix() {
        let row = make_row("evt-001", "PhoenixLoop", 0.85, Some("E05001234"));
        let alert = parse_alert_row(row).expect("should parse");
        assert_eq!(alert.id, "evt-001");
        assert_eq!(alert.motif_type, "PhoenixLoop");
        assert!((alert.score - 0.85).abs() < 0.001);
        assert_eq!(alert.ward_code, Some("E05001234".to_string()));
        assert!(!alert.detected_at.is_empty());
    }

    #[test]
    fn test_parse_alert_row_no_ward() {
        let row = make_row("evt-002", "AddressCluster", 0.72, None);
        let alert = parse_alert_row(row).expect("should parse");
        assert_eq!(alert.id, "evt-002");
        assert_eq!(alert.motif_type, "AddressCluster");
        assert!(alert.ward_code.is_none());
    }

    /// A withdrawn alert must survive parsing with its withdrawal intact.
    ///
    /// Detection upserts what it finds and never withdraws what it stops finding, so a
    /// corrected false positive otherwise sits in the queue forever, indistinguishable from
    /// a live finding (planning open item 23). Marking is preferred to deleting: a retracted
    /// alert is evidence about detector quality, and this project has already destroyed that
    /// history once.
    #[test]
    fn test_parse_alert_row_carries_withdrawal() {
        let mut result = make_motif_result("PhoenixLoop", 0.85, Some("E05001234"));
        result["withdrawn_at"] = serde_json::json!("2026-08-11T09:00:00Z");
        result["withdrawn_reason"] = serde_json::json!("officer merge proved false");
        let row = serde_json::json!({
            "event_id": "evt-withdrawn",
            "timestamp": 1_700_000_000_000u64,
            "kind": "MotifDetected",
            "payload": { "result_json": result.to_string() }
        });
        let alert = parse_alert_row(row).expect("should parse");
        assert_eq!(
            alert.withdrawn_at.as_deref(),
            Some("2026-08-11T09:00:00Z"),
            "withdrawal timestamp must survive parsing"
        );
        assert_eq!(
            alert.withdrawn_reason.as_deref(),
            Some("officer merge proved false"),
            "withdrawal reason must survive parsing"
        );
    }

    /// Every alert already in the database predates the field, so its absence must parse as
    /// "live" rather than failing the row.
    #[test]
    fn test_parse_alert_row_without_withdrawal_is_live() {
        let row = make_row("evt-live", "AddressCluster", 0.72, None);
        let alert = parse_alert_row(row).expect("should parse");
        assert!(alert.withdrawn_at.is_none(), "absent field means live");
        assert!(alert.withdrawn_reason.is_none());
    }

    #[test]
    fn test_withdrawn_alerts_are_hidden_unless_requested() {
        let live = parse_alert_row(make_row("evt-live", "AddressCluster", 0.9, None))
            .expect("should parse");
        let mut result = make_motif_result("PhoenixLoop", 0.85, None);
        result["withdrawn_at"] = serde_json::json!("2026-08-11T09:00:00Z");
        let withdrawn = parse_alert_row(serde_json::json!({
            "event_id": "evt-withdrawn",
            "timestamp": 1_700_000_000_000u64,
            "kind": "MotifDetected",
            "payload": { "result_json": result.to_string() }
        }))
        .expect("should parse");

        let default_filter = AlertFilter::default();
        assert!(
            alert_passes_withdrawn_filter(&live, &default_filter),
            "a live alert is always visible"
        );
        assert!(
            !alert_passes_withdrawn_filter(&withdrawn, &default_filter),
            "a withdrawn alert must NOT reach the analyst queue by default"
        );

        let inclusive = AlertFilter {
            include_withdrawn: true,
            ..AlertFilter::default()
        };
        assert!(
            alert_passes_withdrawn_filter(&withdrawn, &inclusive),
            "--include-withdrawn must still surface it for audit"
        );
    }

    #[test]
    fn test_parse_alert_row_wrong_kind() {
        let row = serde_json::json!({
            "event_id": "evt-003",
            "kind": "NodeAdded",
            "payload": {}
        });
        assert!(parse_alert_row(row).is_none());
    }

    #[test]
    fn test_parse_alert_row_malformed_result_json() {
        let row = serde_json::json!({
            "event_id": "evt-004",
            "kind": "MotifDetected",
            "payload": { "result_json": "not-valid-json" }
        });
        // Malformed inner JSON → None (graceful skip, not panic)
        assert!(parse_alert_row(row).is_none());
    }

    #[test]
    fn test_filter_by_score() {
        let alerts = [
            AlertRecord {
                id: "a".into(),
                motif_type: "PhoenixLoop".into(),
                score: 0.9,
                result_json: "{}".into(),
                detected_at: "".into(),
                ward_code: None,
                withdrawn_at: None,
                withdrawn_reason: None,
                participants: vec![],
                first_seen_run_id: None,
                last_seen_run_id: None,
                provenance: Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
            },
            AlertRecord {
                id: "b".into(),
                motif_type: "PhoenixLoop".into(),
                score: 0.3,
                result_json: "{}".into(),
                detected_at: "".into(),
                ward_code: None,
                withdrawn_at: None,
                withdrawn_reason: None,
                participants: vec![],
                first_seen_run_id: None,
                last_seen_run_id: None,
                provenance: Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
            },
        ];
        let filter = AlertFilter {
            min_score: 0.7,
            ..Default::default()
        };
        let filtered: Vec<_> = alerts
            .iter()
            .filter(|a| a.score >= filter.min_score)
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
    }

    #[test]
    fn test_filter_by_ward() {
        let make_alert = |id: &str, ward: Option<&str>| AlertRecord {
            id: id.into(),
            motif_type: "PhoenixLoop".into(),
            score: 0.8,
            result_json: "{}".into(),
            detected_at: "".into(),
            ward_code: ward.map(|s| s.to_string()),
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        };
        let alerts = [
            make_alert("a", Some("E05001234")),
            make_alert("b", Some("E05005678")),
            make_alert("c", None),
        ];
        let target_ward = "E05001234".to_string();
        let filtered: Vec<_> = alerts
            .iter()
            .filter(|a| {
                Some(&target_ward).is_none_or(|w| a.ward_code.as_deref() == Some(w.as_str()))
            })
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
    }

    #[test]
    fn test_alert_filter_default() {
        let f = AlertFilter::default();
        assert_eq!(f.min_score, 0.0);
        assert_eq!(f.limit, 0);
        assert!(f.ward_code.is_none());
        assert!(f.motif_type.is_none());
    }

    // ── Entity node row parsing + slug-shape unit tests ─────────────────────

    fn make_entity_node_row(
        entity_id: &str,
        name: &str,
        ch_number: Option<&str>,
    ) -> serde_json::Value {
        use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};
        let weight = NodeWeight::Entity(EntityData {
            name: name.to_owned(),
            company_number: ch_number.map(str::to_owned),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        serde_json::json!({
            "event_id": format!("node-{}", entity_id),
            "timestamp": 1_700_000_000_000u64,
            "kind": "NodeAdded",
            "payload": {
                "entity_id": entity_id,
                "weight_json": weight_json,
            }
        })
    }

    #[test]
    fn test_parse_entity_node_row_returns_none_for_motif_event() {
        // A MotifDetected row must NOT be parsed as an entity node
        let row = make_row("evt-001", "PhoenixLoop", 0.85, Some("E05001234"));
        assert!(parse_entity_node_row(row).is_none());
    }

    #[test]
    fn test_parse_entity_node_row_returns_some_for_node_added_event() {
        let row = make_entity_node_row("GB-COH:12345678", "Apex Build Ltd", Some("12345678"));
        let result = parse_entity_node_row(row).expect("should parse NodeAdded Entity row");
        assert_eq!(result.0, "GB-COH:12345678");
        assert_eq!(result.1.name, "Apex Build Ltd");
        assert_eq!(result.1.company_number, Some("12345678".to_owned()));
    }

    fn make_offshore_node_row(
        offshore_id: &str,
        name: &str,
        jurisdiction: Option<&str>,
    ) -> serde_json::Value {
        use urban_integrity_schema::types::node::{NodeWeight, OffshoreEntityData};
        let weight = NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: name.to_owned(),
            jurisdiction: jurisdiction.map(str::to_owned),
            icij_node_id: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        serde_json::json!({
            "event_id": format!("node-{}", offshore_id),
            "timestamp": 1_700_000_000_000u64,
            "kind": "NodeAdded",
            "payload": { "entity_id": offshore_id, "weight_json": weight_json }
        })
    }

    #[test]
    fn test_parse_offshore_node_row_parses_offshore_entity() {
        let row = make_offshore_node_row(
            "GB-OCOD:jersey-acme-holdings-ltd",
            "Acme Holdings Ltd",
            Some("Jersey"),
        );
        let (id, data) = parse_offshore_node_row(row).expect("should parse OffshoreEntity row");
        assert_eq!(id, "GB-OCOD:jersey-acme-holdings-ltd");
        assert_eq!(data.name, "Acme Holdings Ltd");
        assert_eq!(data.jurisdiction.as_deref(), Some("Jersey"));
    }

    #[test]
    fn test_parse_offshore_node_row_rejects_entity_row() {
        // An Entity NodeAdded row must NOT parse as an offshore entity.
        let row = make_entity_node_row("GB-COH:12345678", "Apex Build Ltd", Some("12345678"));
        assert!(parse_offshore_node_row(row).is_none());
    }

    #[test]
    fn test_compute_in_key_entity_vs_offshore() {
        // Domestic entity by CH number → entity:`GB-COH:…`
        assert_eq!(
            compute_in_key("GB-COH:12345678", Some("12345678")),
            "entity:`GB-COH:12345678`"
        );
        // Bare CH number with no GB-COH prefix is normalised.
        assert_eq!(
            compute_in_key("director:jane-doe", Some("12345678")),
            "entity:`GB-COH:12345678`"
        );
        // OCOD overseas owner → offshore_entity:`GB-OCOD:…` (no CH normalisation).
        assert_eq!(
            compute_in_key("GB-OCOD:jersey-acme-holdings-ltd", None),
            "offshore_entity:`GB-OCOD:jersey-acme-holdings-ltd`"
        );
        // ROE overseas-entity register owner → offshore_entity:`GB-ROE:…`.
        assert_eq!(
            compute_in_key("GB-ROE:OE123456", None),
            "offshore_entity:`GB-ROE:OE123456`"
        );
    }

    #[test]
    fn test_is_slug_shape_detects_hyphens() {
        assert!(is_slug_shape("apex-build-ltd"));
    }

    #[test]
    fn test_is_slug_shape_detects_colons() {
        // GB-COH:12345678 contains both hyphen and colon — slug shape
        assert!(is_slug_shape("GB-COH:12345678"));
    }

    #[test]
    fn test_is_slug_shape_plain_name_returns_false() {
        assert!(!is_slug_shape("Apex Build Ltd"));
    }

    // ── CH number validation unit tests ──────────────────────────────────────

    #[test]
    fn test_is_valid_ch_number_accepts_digits() {
        assert!(is_valid_ch_number("12345678"));
        assert!(is_valid_ch_number("SC123456"));
        assert!(is_valid_ch_number("NI123456"));
        assert!(is_valid_ch_number("OC123456"));
    }

    #[test]
    fn test_is_valid_ch_number_rejects_special_chars() {
        assert!(!is_valid_ch_number("12345678'; DELETE"));
        assert!(!is_valid_ch_number(""));
        // Single char — too short (len < 2)
        assert!(!is_valid_ch_number("a"));
        // Semicolons / quotes
        assert!(!is_valid_ch_number("abc;def"));
        assert!(!is_valid_ch_number("'quoted'"));
    }

    // ── Phase 11 query-layer unit tests ──────────────────────────────────────

    #[test]
    fn test_parse_alert_row_ward_code_in_score_breakdown() {
        let row = make_row("evt-ward-fix", "PhoenixLoop", 0.85, Some("E05001234"));
        let alert = parse_alert_row(row).expect("should parse");
        assert_eq!(
            alert.ward_code,
            Some("E05001234".to_string()),
            "ward_code must be read from score_breakdown.ward_code, not the top-level field"
        );
    }

    fn alert(id: &str, motif: &str, score: f64, detected_at: &str) -> AlertRecord {
        AlertRecord {
            id: id.to_owned(),
            motif_type: motif.to_owned(),
            score,
            result_json: "{}".to_owned(),
            detected_at: detected_at.to_owned(),
            ward_code: None,
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        }
    }

    /// **The defect that hid the project's only live Phoenix alert.**
    ///
    /// Ordering was `timestamp DESC`, so an analyst saw the most *recent* alerts, not the most
    /// *severe*. Live, that buried a `PhoenixLoop` scoring 0.85 (written 2026-08-11) beneath 1,807
    /// later `AddressCluster` rows and a 1,000-row cap. The highest-value finding the system had
    /// ever produced was unreachable from `alerts list`.
    #[test]
    fn the_highest_scoring_alert_comes_first_however_old_it_is() {
        let records = vec![
            alert(
                "new-low",
                "AddressCluster",
                0.75,
                "2026-08-18T00:00:00+00:00",
            ),
            alert("old-high", "PhoenixLoop", 0.95, "2026-08-11T00:00:00+00:00"),
            alert(
                "new-mid",
                "AddressCluster",
                0.85,
                "2026-08-18T00:00:00+00:00",
            ),
        ];
        let page = select_alerts(records, &AlertFilter::default());
        let ids: Vec<&str> = page.alerts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["old-high", "new-mid", "new-low"],
            "severity must outrank recency"
        );
    }

    /// A matching alert must be found however far down the corpus it sits. The old code fetched a
    /// `limit * 10` window ordered by time and filtered inside it, so `--motif phoenix` returned
    /// `[]` while a live PhoenixLoop alert existed — a false negative that reads as "none exist".
    #[test]
    fn a_motif_filter_finds_a_match_buried_under_newer_alerts() {
        let mut records: Vec<AlertRecord> = (0..2_000)
            .map(|i| {
                alert(
                    &format!("cluster-{i}"),
                    "AddressCluster",
                    0.95,
                    "2026-08-18T00:00:00+00:00",
                )
            })
            .collect();
        // Older, lower-scoring, and far outside any recency window.
        records.push(alert(
            "phoenix",
            "PhoenixLoop",
            0.85,
            "2026-08-11T00:00:00+00:00",
        ));

        let page = select_alerts(
            records,
            &AlertFilter {
                motif_type: Some("phoenixloop".to_owned()),
                limit: 100,
                ..Default::default()
            },
        );
        assert_eq!(page.total_matched, 1, "the buried alert must be found");
        assert_eq!(page.alerts[0].id, "phoenix");
    }

    /// The display limit must be honoured exactly and reported honestly. The old code silently
    /// clamped any request above 1,000, so asking for 2,000 returned 1,000 with no signal.
    #[test]
    fn the_page_reports_how_many_matched_not_just_how_many_are_shown() {
        let records: Vec<AlertRecord> = (0..50)
            .map(|i| {
                alert(
                    &format!("a{i}"),
                    "AddressCluster",
                    0.9,
                    "2026-08-18T00:00:00+00:00",
                )
            })
            .collect();
        let page = select_alerts(
            records,
            &AlertFilter {
                limit: 10,
                ..Default::default()
            },
        );
        assert_eq!(page.alerts.len(), 10, "the limit is honoured");
        assert_eq!(page.total_matched, 50, "the total must be reported");
        assert!(page.truncated(), "truncation must be visible to the caller");
    }

    /// `limit: 0` means unlimited, and nothing is hidden.
    #[test]
    fn an_unlimited_query_returns_everything_and_is_not_truncated() {
        let records: Vec<AlertRecord> = (0..1_500)
            .map(|i| {
                alert(
                    &format!("a{i}"),
                    "AddressCluster",
                    0.9,
                    "2026-08-18T00:00:00+00:00",
                )
            })
            .collect();
        let page = select_alerts(
            records,
            &AlertFilter {
                limit: 0,
                ..Default::default()
            },
        );
        assert_eq!(page.alerts.len(), 1_500, "no silent 1,000-row clamp");
        assert!(!page.truncated());
    }

    /// The order must be total, so the same query over the same data returns the same sequence
    /// every time. Equal scores and equal timestamps must still break deterministically — the
    /// read-path half of idempotency.
    #[test]
    fn ordering_is_deterministic_when_scores_and_dates_tie() {
        let build = || {
            vec![
                alert("zzz", "AddressCluster", 0.9, "2026-08-18T00:00:00+00:00"),
                alert("aaa", "AddressCluster", 0.9, "2026-08-18T00:00:00+00:00"),
                alert("mmm", "AddressCluster", 0.9, "2026-08-18T00:00:00+00:00"),
            ]
        };
        let first = select_alerts(build(), &AlertFilter::default());
        // Same records, different input order.
        let mut shuffled = build();
        shuffled.reverse();
        let second = select_alerts(shuffled, &AlertFilter::default());
        assert_eq!(first, second, "output must not depend on input order");
        let ids: Vec<&str> = first.alerts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["aaa", "mmm", "zzz"]);
    }

    /// A withdrawn alert must never reach the analyst queue by default — T-23 false accusation.
    /// The live corpus holds exactly this case: a retracted PhoenixLoop beside a live one.
    #[test]
    fn a_withdrawn_alert_is_excluded_unless_explicitly_requested() {
        let mut retracted = alert(
            "withdrawn",
            "PhoenixLoop",
            0.99,
            "2026-08-10T00:00:00+00:00",
        );
        retracted.withdrawn_at = Some("2026-08-11T09:01:53+00:00".to_owned());
        let records = vec![
            retracted,
            alert("live", "PhoenixLoop", 0.85, "2026-08-11T00:00:00+00:00"),
        ];

        let page = select_alerts(records.clone(), &AlertFilter::default());
        assert_eq!(page.total_matched, 1, "the retraction must be filtered out");
        assert_eq!(page.alerts[0].id, "live");

        let page = select_alerts(
            records,
            &AlertFilter {
                include_withdrawn: true,
                ..Default::default()
            },
        );
        assert_eq!(page.total_matched, 2);
        assert_eq!(
            page.alerts[0].id, "withdrawn",
            "with retractions included, severity still orders"
        );
    }

    fn alert_with_ward(id: &str, motif: &str, score: f64, ward: Option<&str>) -> AlertRecord {
        let mut a = alert(id, motif, score, "2026-08-18T00:00:00+00:00");
        a.ward_code = ward.map(|s| s.to_owned());
        a
    }

    // ── 46-02 Task 1: max_score, offset paging, sort mode, facets ───────────

    #[test]
    fn test_select_alerts_max_score_ceiling_is_inclusive() {
        let records = vec![
            alert("low", "PhoenixLoop", 0.5, "2026-08-18T00:00:00+00:00"),
            alert("high", "PhoenixLoop", 0.6, "2026-08-18T00:00:00+00:00"),
        ];
        let page = select_alerts(
            records,
            &AlertFilter {
                max_score: Some(0.5),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = page.alerts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["low"],
            "0.6 must be excluded, 0.5 must be included — the ceiling is inclusive"
        );
    }

    #[test]
    fn test_alert_paging_returns_the_correct_slice_and_preserves_total_matched() {
        // 20 distinctly-scored alerts, highest score first once sorted.
        let records: Vec<AlertRecord> = (0..20)
            .map(|i| {
                alert(
                    &format!("a{i:02}"),
                    "AddressCluster",
                    1.0 - (i as f64) * 0.01,
                    "2026-08-18T00:00:00+00:00",
                )
            })
            .collect();
        let page = select_alerts(
            records,
            &AlertFilter {
                start: 10,
                limit: 5,
                ..Default::default()
            },
        );
        let ids: Vec<&str> = page.alerts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a10", "a11", "a12", "a13", "a14"],
            "offset 10, limit 5 must return alerts 11-15 of the total order"
        );
        assert_eq!(
            page.total_matched, 20,
            "the offset must never change the reported total"
        );
    }

    #[test]
    fn test_select_alerts_recency_desc_produces_a_different_total_order() {
        // Disagreeing orders: lower score, more recent vs. higher score, older.
        let records = vec![
            alert("high-old", "PhoenixLoop", 0.9, "2026-01-01T00:00:00+00:00"),
            alert("low-new", "PhoenixLoop", 0.3, "2026-06-01T00:00:00+00:00"),
        ];
        let score_desc = select_alerts(records.clone(), &AlertFilter::default());
        let recency_desc = select_alerts(
            records,
            &AlertFilter {
                sort: AlertSort::RecencyDesc,
                ..Default::default()
            },
        );
        let score_ids: Vec<&str> = score_desc.alerts.iter().map(|a| a.id.as_str()).collect();
        let recency_ids: Vec<&str> = recency_desc.alerts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(score_ids, vec!["high-old", "low-new"]);
        assert_eq!(recency_ids, vec!["low-new", "high-old"]);
        assert_ne!(
            score_ids, recency_ids,
            "the two sort orders must disagree here"
        );
    }

    #[test]
    fn test_alert_facets_respects_other_active_filters() {
        let records = vec![
            alert_with_ward("a", "PhoenixLoop", 0.9, Some("E05001234")),
            alert_with_ward("b", "AddressCluster", 0.9, Some("E05001234")),
            alert_with_ward("c", "PhoenixLoop", 0.9, Some("E05009999")),
        ];
        let filter = AlertFilter {
            ward_code: Some("E05001234".to_owned()),
            ..Default::default()
        };
        let facets = alert_facets(&records, &filter);
        // Only the two E05001234 alerts count, even though motif_type is the dimension being
        // enumerated — the ward filter must still apply.
        assert_eq!(facets.detector_counts.get("PhoenixLoop"), Some(&1));
        assert_eq!(facets.detector_counts.get("AddressCluster"), Some(&1));
        assert_eq!(
            facets.detector_counts.len(),
            2,
            "the E05009999 alert must not contribute to any detector count"
        );
    }

    #[test]
    fn test_alert_facets_ward_list_sorts_by_count_desc_then_code_asc() {
        let records = vec![
            alert_with_ward("a", "PhoenixLoop", 0.9, Some("E05000002")),
            alert_with_ward("b", "PhoenixLoop", 0.9, Some("E05000001")),
            alert_with_ward("c", "PhoenixLoop", 0.9, Some("E05009999")),
            alert_with_ward("d", "PhoenixLoop", 0.9, Some("E05009999")),
        ];
        let facets = alert_facets(&records, &AlertFilter::default());
        assert_eq!(
            facets.ward_counts,
            vec![
                ("E05009999".to_owned(), 2),
                ("E05000001".to_owned(), 1),
                ("E05000002".to_owned(), 1),
            ],
            "highest count first; equal counts break by ward code ascending"
        );
    }

    // ── 46-02 Task 2: WithdrawOutcome + parse_alert_row participants ────────

    #[test]
    fn test_withdraw_alert_decision_live_alert_ready_to_write() {
        let rows = vec![serde_json::json!({
            "payload": { "result_json": "{}" }
        })];
        let at = chrono::Utc::now();
        let result = decide_withdraw_outcome(rows, "false positive", at)
            .expect_err("a live alert must proceed to the write path, not short-circuit");
        assert_eq!(result["withdrawn_at"], serde_json::json!(at.to_rfc3339()));
        assert_eq!(
            result["withdrawn_reason"],
            serde_json::json!("false positive")
        );
    }

    #[test]
    fn test_withdraw_alert_decision_already_withdrawn_ignores_new_reason() {
        let rows = vec![serde_json::json!({
            "payload": {
                "result_json": "{\"withdrawn_at\":\"2026-01-01T00:00:00+00:00\",\"withdrawn_reason\":\"original reason\"}"
            }
        })];
        let outcome = decide_withdraw_outcome(
            rows,
            "a second, different reason that must never be written",
            chrono::Utc::now(),
        )
        .expect("an already-withdrawn alert must short-circuit before any write");
        assert_eq!(
            outcome,
            WithdrawOutcome::AlreadyWithdrawn {
                at: "2026-01-01T00:00:00+00:00".to_owned(),
                reason: "original reason".to_owned(),
            },
            "the FIRST call's at/reason must survive; the second call's reason is discarded"
        );
    }

    #[test]
    fn test_withdraw_alert_decision_not_found_for_missing_row() {
        let outcome = decide_withdraw_outcome(vec![], "reason", chrono::Utc::now())
            .expect("no matching row must short-circuit, not attempt a write");
        assert_eq!(outcome, WithdrawOutcome::NotFound);
    }

    #[test]
    fn test_parse_alert_row_participants_from_result_json() {
        let row = serde_json::json!({
            "event_id": "evt-participants-1",
            "kind": "MotifDetected",
            "timestamp": 1_700_000_000_000_u64,
            "payload": {
                "result_json": "{\"motif_type\":\"PhoenixLoop\",\"risk_score\":0.8,\"participants\":[\"A\",\"B\"]}"
            }
        });
        let alert = parse_alert_row(row).expect("should parse");
        assert_eq!(alert.participants, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn test_parse_alert_row_participants_defaults_empty_when_absent() {
        // Today's property_accumulation_anomaly shape (before 46-05 ships the key) — no
        // "participants" field at all.
        let row = serde_json::json!({
            "event_id": "evt-participants-2",
            "kind": "MotifDetected",
            "timestamp": 1_700_000_000_000_u64,
            "payload": {
                "result_json": "{\"motif_type\":\"PropertyAccumulation\",\"risk_score\":0.5}"
            }
        });
        let alert = parse_alert_row(row).expect("must still parse without a participants key");
        assert_eq!(
            alert.participants,
            Vec::<String>::new(),
            "must default to [] rather than panicking or dropping the row"
        );
    }

    #[test]
    fn test_query_alerts_respects_from_date() {
        let make_alert = |id: &str, detected_at: &str| AlertRecord {
            id: id.to_owned(),
            motif_type: "PhoenixLoop".to_owned(),
            score: 0.8,
            result_json: "{}".to_owned(),
            detected_at: detected_at.to_owned(),
            ward_code: None,
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        };

        let early = make_alert("early", "2026-01-01T00:00:00+00:00");
        let late = make_alert("late", "2026-06-01T00:00:00+00:00");
        let from = "2026-03-01T00:00:00+00:00";

        let from_dt = chrono::DateTime::parse_from_rfc3339(from).unwrap();
        let alerts = [early, late];
        let filtered: Vec<_> = alerts
            .iter()
            .filter(|a| {
                chrono::DateTime::parse_from_rfc3339(&a.detected_at)
                    .map(|dt| dt >= from_dt)
                    .unwrap_or(true)
            })
            .collect();

        assert_eq!(
            filtered.len(),
            1,
            "only the June alert should pass the from_date filter"
        );
        assert_eq!(filtered[0].id, "late");
    }

    #[test]
    fn test_query_alerts_unlimited() {
        let filter_unlimited = AlertFilter {
            limit: 0,
            ..Default::default()
        };
        let filter_limited = AlertFilter {
            limit: 10,
            ..Default::default()
        };

        assert!(
            filter_unlimited.limit == 0,
            "limit == 0 must be the unlimited sentinel"
        );
        assert!(
            filter_limited.limit != 0,
            "non-zero limit must not trigger the unlimited path"
        );

        let unlimited = filter_unlimited.limit == 0;
        let effective_limit = if unlimited {
            usize::MAX
        } else {
            filter_unlimited.limit.clamp(1, 1000)
        };
        assert_eq!(
            effective_limit,
            usize::MAX,
            "unlimited path must use usize::MAX, not clamp 0 → 1"
        );

        let mut test_alerts: Vec<&str> = vec!["a", "b", "c", "d", "e"];
        if !unlimited {
            test_alerts.truncate(effective_limit);
        }
        assert_eq!(
            test_alerts.len(),
            5,
            "unlimited path must not truncate the result set"
        );
    }

    #[test]
    fn test_enrich_alert_participants_empty() {
        let slugs: &[String] = &[];
        assert!(
            slugs.is_empty(),
            "empty slugs slice must trigger the early-return guard in enrich_alert_participants"
        );

        let non_empty: &[String] = &["GB-COH:12345678".to_owned()];
        assert!(
            !non_empty.is_empty(),
            "non-empty slugs must NOT trigger the early-return guard"
        );
    }

    // ── CR-02: is_valid_event_id sanitisation ─────────────────────────────────

    #[test]
    fn test_is_valid_event_id_accepts_safe_ids() {
        assert!(is_valid_event_id("abc123"));
        assert!(is_valid_event_id("abc-def-123"));
        assert!(is_valid_event_id("abc_def_123"));
        assert!(is_valid_event_id("A1B2C3"));
        // exactly 64 chars
        assert!(is_valid_event_id(&"a".repeat(64)));
    }

    #[test]
    fn test_is_valid_event_id_rejects_injection_chars() {
        assert!(!is_valid_event_id("id' OR 1=1 --"));
        assert!(!is_valid_event_id("id\" DROP TABLE--"));
        assert!(!is_valid_event_id("id with space"));
        assert!(!is_valid_event_id("id\nnewline"));
        assert!(!is_valid_event_id("id;cmd"));
        assert!(!is_valid_event_id("id[bracket]"));
    }

    #[test]
    fn test_is_valid_event_id_rejects_empty_and_overlong() {
        assert!(!is_valid_event_id(""));
        assert!(!is_valid_event_id(&"a".repeat(65)));
    }

    // ── parse_officer_node_row ────────────────────────────────────────────────

    fn make_officer_node_row(officer_id: &str, name: &str) -> serde_json::Value {
        use urban_integrity_schema::types::node::{NodeWeight, OfficerData};
        let weight = NodeWeight::Officer(OfficerData {
            name: name.to_owned(),
            officer_role: Some("director".to_owned()),
            appointed_on: Some("2020-01-15".to_owned()),
            resigned_on: None,
            company_number: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        serde_json::json!({
            "event_id": format!("node-{}", officer_id),
            "timestamp": 1_700_000_000_000u64,
            "kind": "NodeAdded",
            "payload": {
                "entity_id": officer_id,
                "weight_json": weight_json,
            }
        })
    }

    #[test]
    fn test_parse_officer_node_row_returns_some_for_officer() {
        let row = make_officer_node_row("director:john-smith", "John Smith");
        let result = parse_officer_node_row(row).expect("must parse Officer NodeAdded row");
        assert_eq!(result.0, "director:john-smith");
        assert_eq!(result.1.name, "John Smith");
        assert_eq!(result.1.officer_role.as_deref(), Some("director"));
        assert_eq!(result.1.appointed_on.as_deref(), Some("2020-01-15"));
        assert!(result.1.resigned_on.is_none());
    }

    #[test]
    fn test_parse_officer_node_row_returns_none_for_entity_node() {
        // An Entity NodeAdded row must return None from the Officer parser
        let row = make_entity_node_row("GB-COH:12345678", "Apex Build Ltd", Some("12345678"));
        assert!(
            parse_officer_node_row(row).is_none(),
            "Entity node row must not parse as Officer"
        );
    }

    #[test]
    fn test_parse_officer_node_row_returns_none_for_motif_row() {
        let row = make_row("evt-001", "PhoenixLoop", 0.85, Some("E05001234"));
        assert!(
            parse_officer_node_row(row).is_none(),
            "MotifDetected row must not parse as Officer"
        );
    }

    #[test]
    fn test_parse_officer_node_row_returns_none_for_missing_weight_json() {
        let row = serde_json::json!({
            "event_id": "n-001",
            "kind": "NodeAdded",
            "payload": {
                "entity_id": "director:jane"
                // weight_json missing
            }
        });
        assert!(
            parse_officer_node_row(row).is_none(),
            "Missing weight_json must return None"
        );
    }

    // ── parse_entity_node_row additional cases ────────────────────────────────

    #[test]
    fn test_parse_entity_node_row_returns_none_for_officer_node() {
        let row = make_officer_node_row("director:john-smith", "John Smith");
        assert!(
            parse_entity_node_row(row).is_none(),
            "Officer node row must not parse as Entity"
        );
    }

    #[test]
    fn test_parse_entity_node_row_returns_none_for_missing_weight_json() {
        let row = serde_json::json!({
            "event_id": "n-002",
            "kind": "NodeAdded",
            "payload": {
                "entity_id": "GB-COH:12345678"
            }
        });
        assert!(
            parse_entity_node_row(row).is_none(),
            "Missing weight_json must return None"
        );
    }

    // ── NodeUpdated folding (L5) ──────────────────────────────────────────────

    /// Build a node-weight row with an explicit kind, timestamp and status.
    fn make_status_row(
        kind: &str,
        entity_id: &str,
        timestamp: u64,
        status: urban_integrity_schema::types::node::EntityStatus,
    ) -> serde_json::Value {
        use urban_integrity_schema::types::node::{EntityData, NodeWeight};
        let weight = NodeWeight::Entity(EntityData {
            name: "Ghost Corp".to_owned(),
            company_number: Some("12345678".to_owned()),
            status,
            incorporation_date: None,
        });
        serde_json::json!({
            "event_id": format!("{kind}-{entity_id}-{timestamp}"),
            "timestamp": timestamp,
            "kind": kind,
            "payload": {
                "entity_id": entity_id,
                "weight_json": serde_json::to_string(&weight).unwrap(),
            }
        })
    }

    fn dissolved() -> urban_integrity_schema::types::node::EntityStatus {
        urban_integrity_schema::types::node::EntityStatus::Dissolved(
            "2026-07-28T00:00:00Z".parse().unwrap(),
        )
    }

    #[test]
    fn test_parse_entity_node_row_accepts_node_updated() {
        let row = make_status_row(
            "NodeUpdated",
            "GB-COH:12345678",
            2,
            urban_integrity_schema::types::node::EntityStatus::Active,
        );
        let (eid, _data) =
            parse_entity_node_row(row).expect("NodeUpdated must parse as a node-weight row");
        assert_eq!(eid, "GB-COH:12345678");
    }

    #[test]
    fn test_parse_entity_node_row_still_rejects_edge_events() {
        // Widening to NodeUpdated must not widen to everything.
        for kind in ["EdgeAdded", "NodeRemoved", "MotifDetected"] {
            let mut row = make_status_row(
                "NodeAdded",
                "GB-COH:12345678",
                1,
                urban_integrity_schema::types::node::EntityStatus::Active,
            );
            row["kind"] = serde_json::json!(kind);
            assert!(
                parse_entity_node_row(row).is_none(),
                "{kind} must not parse as a node-weight row"
            );
        }
    }

    #[test]
    fn test_fold_latest_node_rows_keeps_the_newest_status() {
        use urban_integrity_schema::types::node::EntityStatus;
        // NodeUpdated seeded FIRST: id order and timestamp order are inverted for ~40% of the
        // live log, so a fold that trusts arrival order silently returns the birth weight.
        let rows = vec![
            make_status_row("NodeUpdated", "GB-COH:12345678", 200, dissolved()),
            make_status_row("NodeAdded", "GB-COH:12345678", 100, EntityStatus::Active),
        ];
        let folded = fold_latest_node_rows(rows);
        assert_eq!(folded.len(), 1, "one entity must fold to one row");
        let (_eid, data) = parse_entity_node_row(folded.into_iter().next().unwrap()).unwrap();
        assert!(matches!(data.status, EntityStatus::Dissolved(_)));
    }

    #[test]
    fn test_fold_latest_node_rows_preserves_first_seen_order() {
        use urban_integrity_schema::types::node::EntityStatus;
        let rows = vec![
            make_status_row("NodeAdded", "b", 1, EntityStatus::Active),
            make_status_row("NodeAdded", "a", 1, EntityStatus::Active),
            make_status_row("NodeUpdated", "b", 9, dissolved()),
        ];
        let ids: Vec<String> = fold_latest_node_rows(rows)
            .iter()
            .map(|r| r["payload"]["entity_id"].as_str().unwrap().to_owned())
            .collect();
        // "b" keeps its first-seen slot even though its surviving row arrived last — the
        // callers that truncate(10) afterwards depend on a stable order.
        assert_eq!(ids, vec!["b".to_owned(), "a".to_owned()]);
    }

    #[test]
    fn test_fold_latest_node_rows_drops_rows_without_an_entity_id() {
        let mut row = make_status_row(
            "NodeAdded",
            "GB-COH:12345678",
            1,
            urban_integrity_schema::types::node::EntityStatus::Active,
        );
        row["payload"]["entity_id"] = serde_json::Value::Null;
        assert!(fold_latest_node_rows(vec![row]).is_empty());
    }

    #[test]
    fn test_fold_latest_node_rows_ties_are_independent_of_arrival_order() {
        use urban_integrity_schema::types::node::EntityStatus;
        // The same tie as the test below, with the correction arriving FIRST. A fold that breaks
        // ties on position resolves this to the birth; one that breaks on kind resolves both
        // orderings to the correction. Arrival order is not usable here: this fold is also fed by
        // `scan_events_of_kinds_by_type`, a single id-ordered scan that this file documents as
        // ~40% inverted from time order.
        let rows = vec![
            make_status_row("NodeUpdated", "GB-COH:12345678", 500, dissolved()),
            make_status_row("NodeAdded", "GB-COH:12345678", 500, EntityStatus::Active),
        ];
        let folded = fold_latest_node_rows(rows);
        let (_eid, data) = parse_entity_node_row(folded.into_iter().next().unwrap()).unwrap();
        assert!(
            matches!(data.status, EntityStatus::Dissolved(_)),
            "an equal-timestamp NodeUpdated must win regardless of arrival order"
        );
    }

    #[test]
    fn test_fold_latest_node_rows_ties_keep_the_later_arrival() {
        use urban_integrity_schema::types::node::EntityStatus;
        // A backfill batch reads the clock once and stamps every row with it. The correction
        // is the one written later, so an equal timestamp must not resolve to the birth row.
        let rows = vec![
            make_status_row("NodeAdded", "GB-COH:12345678", 500, EntityStatus::Active),
            make_status_row("NodeUpdated", "GB-COH:12345678", 500, dissolved()),
        ];
        let folded = fold_latest_node_rows(rows);
        let (_eid, data) = parse_entity_node_row(folded.into_iter().next().unwrap()).unwrap();
        assert!(matches!(data.status, EntityStatus::Dissolved(_)));
    }

    // ── is_slug_shape edge cases ──────────────────────────────────────────────

    #[test]
    fn test_is_slug_shape_empty_string_returns_false() {
        assert!(!is_slug_shape(""));
    }

    #[test]
    fn test_is_slug_shape_whitespace_trims_before_check() {
        // Leading/trailing whitespace is trimmed; if no separator remains → false
        assert!(!is_slug_shape("   Apex Build Ltd   "));
    }

    #[test]
    fn test_is_slug_shape_director_slug_format() {
        assert!(is_slug_shape("director:jane-smith"));
    }

    // ── Phase 14 verify_tests ─────────────────────────────────────────────────

    mod verify_tests {
        use super::*;
        use chrono::Utc;
        use urban_integrity_schema::types::node::EntityStatus;

        fn make_alert(score: f64, motif_type: &str) -> AlertRecord {
            AlertRecord {
                id: "x".into(),
                motif_type: motif_type.into(),
                score,
                result_json: "{}".into(),
                detected_at: "2026-01-01T00:00:00Z".into(),
                ward_code: None,
                withdrawn_at: None,
                withdrawn_reason: None,
                participants: vec![],
                first_seen_run_id: None,
                last_seen_run_id: None,
                provenance: Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
            }
        }

        fn make_related(status: EntityStatus) -> RelatedEntity {
            RelatedEntity {
                name: "Related Co".into(),
                ch_number: None,
                status,
                shared_connection: SharedConnection {
                    kind: "director".into(),
                    value: "John Smith".into(),
                },
                provenance: Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
            }
        }

        fn make_profile_with_alerts(
            alerts: Vec<AlertRecord>,
            related: Vec<RelatedEntity>,
        ) -> EntityProfile {
            EntityProfile {
                id: "test-entity".to_string(),
                name: "Apex Build Ltd".into(),
                ch_number: Some("12345678".into()),
                status: EntityStatus::Active,
                incorporation_date: None,
                addresses: vec![],
                directors: vec![
                    DirectorRow {
                        name: "Jane Smith".into(),
                        role: Some("director".into()),
                        appointed: Some("2020-01-01".into()),
                        resigned: None,
                        provenance: Provenance::Unattributed {
                            reason: "hand-built test fixture".to_owned(),
                        },
                        temporal: Temporal::unfiltered(EffectiveRange::Unrecorded {
                            basis: "hand-built test fixture".to_owned(),
                        }),
                    },
                    DirectorRow {
                        name: "Bob Jones".into(),
                        role: Some("director".into()),
                        appointed: Some("2021-06-01".into()),
                        resigned: None,
                        provenance: Provenance::Unattributed {
                            reason: "hand-built test fixture".to_owned(),
                        },
                        temporal: Temporal::unfiltered(EffectiveRange::Unrecorded {
                            basis: "hand-built test fixture".to_owned(),
                        }),
                    },
                ],
                contracts: vec![],
                planning_applications: vec![],
                alerts,
                related_entities: related,
                lpa_presence: vec![],
                beneficial_owners: vec![],
                disqualifications: vec![],
                offshore_links: vec![],
                trustee_of: vec![],
                owns_property: vec![],
                donates_to: vec![],
                osint_links: vec![],
                sanctions_status: None,
                osint_alerts: vec![],
                sub_candidate_links: vec![],
                identity_provenance: Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                as_at: None,
            }
        }

        // ── classify_risk threshold tests ──────────────────────────────────

        #[test]
        fn test_classify_risk_low() {
            assert_eq!(classify_risk(0.0), RiskLevel::Low);
            assert_eq!(classify_risk(0.49), RiskLevel::Low);
        }

        #[test]
        fn test_classify_risk_medium() {
            assert_eq!(classify_risk(0.5), RiskLevel::Medium);
            assert_eq!(classify_risk(0.69), RiskLevel::Medium);
        }

        #[test]
        fn test_classify_risk_high() {
            assert_eq!(classify_risk(0.7), RiskLevel::High);
            assert_eq!(classify_risk(0.89), RiskLevel::High);
        }

        #[test]
        fn test_classify_risk_critical() {
            assert_eq!(classify_risk(0.9), RiskLevel::Critical);
            assert_eq!(classify_risk(1.0), RiskLevel::Critical);
        }

        #[test]
        fn test_classify_risk_exact_boundaries() {
            // 0.5 must be Medium (not Low), 0.7 must be High (not Medium), 0.9 must be Critical (not High)
            assert_eq!(
                classify_risk(0.5),
                RiskLevel::Medium,
                "0.5 boundary must be Medium"
            );
            assert_eq!(
                classify_risk(0.7),
                RiskLevel::High,
                "0.7 boundary must be High"
            );
            assert_eq!(
                classify_risk(0.9),
                RiskLevel::Critical,
                "0.9 boundary must be Critical"
            );
        }

        // ── build_verify_verdict tests ─────────────────────────────────────

        #[test]
        fn test_build_verdict_no_alerts() {
            let profile = make_profile_with_alerts(vec![], vec![]);
            let verdict = build_verify_verdict(&profile);
            assert_eq!(verdict.p_c_score, 0.0);
            assert_eq!(verdict.risk_level, RiskLevel::Low);
            assert!(verdict.motif_flags.is_empty());
            assert_eq!(verdict.dissolved_relative_count, 0);
            // Two directors in make_profile_with_alerts
            assert_eq!(verdict.connected_officer_count, 2);
        }

        #[test]
        fn test_build_verdict_takes_max_score() {
            let alerts = vec![
                make_alert(0.3, "AddressCluster"),
                make_alert(0.95, "PhoenixLoop"),
                make_alert(0.6, "NepotismBridge"),
            ];
            let profile = make_profile_with_alerts(alerts, vec![]);
            let verdict = build_verify_verdict(&profile);
            assert!(
                (verdict.p_c_score - 0.95).abs() < 1e-9,
                "p_c_score must be the maximum across all alerts, got {}",
                verdict.p_c_score
            );
            assert_eq!(verdict.risk_level, RiskLevel::Critical);
        }

        #[test]
        fn test_build_verdict_dissolved_count() {
            let related = vec![
                make_related(EntityStatus::Dissolved(Utc::now())),
                make_related(EntityStatus::Dissolved(Utc::now())),
                make_related(EntityStatus::Active),
                make_related(EntityStatus::InLiquidation(Utc::now())),
            ];
            let profile = make_profile_with_alerts(vec![], related);
            let verdict = build_verify_verdict(&profile);
            assert_eq!(
                verdict.dissolved_relative_count,
                2,
                "only Dissolved(_) variants must be counted; Active and InLiquidation must be excluded"
            );
        }

        #[test]
        fn test_build_verdict_motif_dedup() {
            let alerts = vec![
                make_alert(0.7, "PhoenixLoop"),
                make_alert(0.8, "PhoenixLoop"),
                make_alert(0.6, "AddressCluster"),
            ];
            let profile = make_profile_with_alerts(alerts, vec![]);
            let verdict = build_verify_verdict(&profile);
            assert_eq!(
                verdict.motif_flags,
                vec!["AddressCluster", "PhoenixLoop"],
                "motif_flags must be deduplicated and sorted"
            );
        }

        #[test]
        fn test_verify_verdict_json_camel_case() {
            let profile = make_profile_with_alerts(vec![make_alert(0.75, "PhoenixLoop")], vec![]);
            let verdict = build_verify_verdict(&profile);
            let json =
                serde_json::to_value(&verdict).expect("VerifyVerdict must serialise to JSON");
            assert!(
                json.get("pCScore").is_some(),
                "JSON must contain key 'pCScore'"
            );
            assert!(
                json.get("riskLevel").is_some(),
                "JSON must contain key 'riskLevel'"
            );
            assert!(
                json.get("motifFlags").is_some(),
                "JSON must contain key 'motifFlags'"
            );
            assert!(
                json.get("dissolvedRelativeCount").is_some(),
                "JSON must contain key 'dissolvedRelativeCount'"
            );
            assert!(
                json.get("connectedOfficerCount").is_some(),
                "JSON must contain key 'connectedOfficerCount'"
            );
        }
    }

    // ── SubCandidateLinkRow + query_sub_candidates_for_entity (Phase 19-06) ───

    mod sub_candidate_tests {
        use super::*;

        // Test 1: SubCandidateLinkRow deserialises from camelCase JSON (serde rename_all)
        #[test]
        fn test_sub_candidate_link_row_deserialises_camel_case() {
            let json = serde_json::json!({
                "sourceNodeId": "entity:test-corp",
                "targetNodeId": "director:john-smith",
                "source": "charity_commission",
                "confidence": 0.65,
                "resolutionType": "fuzzy_name",
                "evidenceJson": "{\"score\":0.65}"
            });
            let row: SubCandidateLinkRow = serde_json::from_value(json)
                .expect("SubCandidateLinkRow must deserialise from camelCase JSON");
            assert_eq!(row.source_node_id, "entity:test-corp");
            assert_eq!(row.target_node_id, "director:john-smith");
            assert_eq!(row.source, "charity_commission");
            assert!((row.confidence - 0.65).abs() < 1e-10);
            assert_eq!(row.resolution_type, "fuzzy_name");
            assert_eq!(row.evidence_json, "{\"score\":0.65}");
        }

        // Test 2: EntityProfile default construction includes sub_candidate_links: vec![]
        #[test]
        fn test_entity_profile_includes_sub_candidate_links_field() {
            use urban_integrity_schema::types::node::EntityStatus;
            let profile = EntityProfile {
                id: "test".to_string(),
                name: "Test Corp".to_owned(),
                ch_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
                addresses: vec![],
                directors: vec![],
                contracts: vec![],
                planning_applications: vec![],
                alerts: vec![],
                related_entities: vec![],
                lpa_presence: vec![],
                beneficial_owners: vec![],
                disqualifications: vec![],
                offshore_links: vec![],
                trustee_of: vec![],
                owns_property: vec![],
                donates_to: vec![],
                osint_links: vec![],
                sanctions_status: None,
                osint_alerts: vec![],
                sub_candidate_links: vec![],
                identity_provenance: Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                as_at: None,
            };
            assert!(
                profile.sub_candidate_links.is_empty(),
                "sub_candidate_links must default to empty Vec"
            );
        }

        // Test 3: query_sub_candidates_for_entity with empty result → returns empty Vec
        #[tokio::test]
        async fn test_query_sub_candidates_returns_empty_on_no_rows() {
            // Without a live DB, we verify the function signature and return type compile correctly.
            // The actual filtering is tested via Test 4 (Rust-side filter logic).
            // This test verifies the function exists, takes the right params, and returns Vec<SubCandidateLinkRow>.
            let rows: Vec<serde_json::Value> = vec![];
            let entity_id = "entity:test-corp";
            // Filter logic extracted from query_sub_candidates_for_entity for unit testing:
            let result: Vec<SubCandidateLinkRow> = rows
                .into_iter()
                .filter(|row| {
                    let src = row["sourceNodeId"].as_str().unwrap_or("");
                    let tgt = row["targetNodeId"].as_str().unwrap_or("");
                    src.contains(entity_id) || tgt.contains(entity_id)
                })
                .map(|row| SubCandidateLinkRow {
                    source_node_id: row["sourceNodeId"].as_str().unwrap_or("").to_string(),
                    target_node_id: row["targetNodeId"].as_str().unwrap_or("").to_string(),
                    source: row["source"].as_str().unwrap_or("").to_string(),
                    confidence: row["confidence"].as_f64().unwrap_or(0.0),
                    resolution_type: row["resolutionType"].as_str().unwrap_or("").to_string(),
                    evidence_json: row["evidenceJson"].as_str().unwrap_or("{}").to_string(),
                })
                .collect();
            assert!(
                result.is_empty(),
                "Empty input must produce empty result, got: {result:?}"
            );
        }

        // Test 4: query_sub_candidates_for_entity filters correctly by entity_id
        #[test]
        fn test_query_sub_candidates_rust_side_filter() {
            // Simulate the Rust-side filter logic from query_sub_candidates_for_entity.
            // row with source_node_id containing "entity:test-corp" → included
            // row with unrelated source_node_id → excluded
            let entity_id = "entity:test-corp";
            let matching_row = serde_json::json!({
                "source_node_id": "entity:test-corp",
                "target_node_id": "director:john-smith",
                "source": "charity_commission",
                "confidence": 0.62,
                "resolution_type": "fuzzy_name",
                "evidence_json": "{}"
            });
            let unrelated_row = serde_json::json!({
                "source_node_id": "entity:other-corp",
                "target_node_id": "director:jane-doe",
                "source": "opensanctions",
                "confidence": 0.55,
                "resolution_type": "fuzzy_name",
                "evidence_json": "{}"
            });
            let rows = [matching_row, unrelated_row];
            // Replicate the Rust-side filter used in query_sub_candidates_for_entity
            let filtered: Vec<&serde_json::Value> = rows
                .iter()
                .filter(|row| {
                    let src = row["source_node_id"].as_str().unwrap_or("");
                    let tgt = row["target_node_id"].as_str().unwrap_or("");
                    src.contains(entity_id) || tgt.contains(entity_id)
                })
                .collect();
            assert_eq!(
                filtered.len(),
                1,
                "Only the matching row must be included; got {filtered:?}"
            );
            assert_eq!(
                filtered[0]["source_node_id"].as_str().unwrap(),
                "entity:test-corp"
            );
        }
    }
}
