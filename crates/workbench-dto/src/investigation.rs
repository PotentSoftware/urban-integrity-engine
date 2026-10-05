//! Saved investigations on the wire (PIV-05) — the mirror plan 45-07 flagged as missing.
//!
//! # Why these types exist at all
//!
//! 45-07 built `saved_investigation` / `saved_investigation_pin` in `crates/core::persistence` and
//! closed with an explicit handoff: *"There is no `workbench-dto` mirror of these types… 45-08's
//! investigation CRUD handlers will either need to add one (a fifth file) or serialise
//! `crates/core` types directly across the HTTP boundary, which every other section of this phase
//! has deliberately not done."*
//!
//! They are added here, for three reasons and not merely for symmetry:
//!
//! 1. **The browser compiles this crate and cannot compile `crates/core`.** 45-15's investigation
//!    UI needs these shapes; `crates/core` pulls in `surrealdb`, `tokio` and `chrono`, every one of
//!    which `scripts/check-wasm-boundary.sh` forbids from the wasm dependency graph.
//! 2. **`SavedInvestigation` carries `chrono::DateTime<Utc>`.** This crate's only dependency is
//!    `serde`, so a timestamp crosses as an RFC 3339 string — which is also what the existing
//!    `Timestamp` component renders.
//! 3. `SavedInvestigation::schema_version` and `analysis_db` are storage facts. They are carried
//!    (a dossier that cannot say which analysis it came from is the failure 45-07's
//!    `session::db()` write exists to prevent) but they are carried *as* a view, not as the
//!    storage type, so a later storage change is not automatically a wire change.
//!
//! # What is deliberately absent
//!
//! An investigation stores **references**, never a copy of the pinned facts (45-07's decision). So
//! there is no `EntityProfileDto` on a pin, no `Provenance` and no `Temporal`: provenance is a
//! property of a *read* and is recomputed each time, and freezing it into an investigation would
//! let an exported dossier contradict the live profile with no way to tell which was right.

use serde::{Deserialize, Serialize};

/// Copied verbatim from `population_meta::required_option` (see that module's doc comment for the
/// full rationale) — this crate's established convention is each DTO module owns its own copy,
/// never a shared import.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// What a pin points at — the wire mirror of `saved_investigations::PinTarget`.
///
/// Two tagged variants rather than one id string with a convention, for the reason the core type
/// gives: an edge is three strings and a direction, and squeezing it into an entity id would make
/// the distinction depend on how a reader parsed a string. The tag values match the core type's
/// `kind()` exactly (`"entity"` / `"edge"`), so the storage column and the wire tag are one
/// vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinTargetView {
    Entity {
        entity_id: String,
    },
    /// A directed edge exactly as it was visible on the canvas. `A -> B` and `B -> A` are two
    /// different facts and are two different pins.
    Edge {
        from: String,
        to: String,
        relation: String,
    },
}

/// Which surface a member entered the set from — mirror of `saved_investigations::PinOrigin`, with
/// the same tokens the storage layer parses strictly.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PinOriginView {
    Profile,
    Canvas,
    Subgraph,
    /// Pinned by hand from the alert inbox (`/alerts`, `/alert/:id`) — the alert's own
    /// primary/hub participant, Phase 46 D-08.
    Alert,
}

/// One saved investigation's own record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedInvestigationView {
    pub id: String,
    pub name: String,
    /// The analysis database this investigation lives in, as the **server** observed it
    /// (`session::db()`, never a caller-supplied string). Carried so an export cannot lose which
    /// analysis it describes.
    pub analysis_db: String,
    pub notes: String,
    pub schema_version: i64,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. The investigation list's "last modified" column.
    pub updated_at: String,
}

/// One pinned member.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedPinView {
    pub id: String,
    pub target: PinTargetView,
    /// The analyst's note on this member. `""` means no note — notes are free text and an empty
    /// string is the honest representation of "nothing typed yet".
    pub note: String,
    pub origin: PinOriginView,
    /// RFC 3339 — when the member was **first** pinned. A re-pin does not move it (45-07), so the
    /// set keeps saying when a member was first found.
    pub pinned_at: String,
    /// RFC 3339.
    pub updated_at: String,
}

/// A row of the investigation list: the record plus its member counts.
///
/// Two counts, not one total: entities and edges are different things to an analyst, and CNV-03's
/// "save the visible subgraph" writes both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvestigationSummaryView {
    pub investigation: SavedInvestigationView,
    pub entity_count: u64,
    pub edge_count: u64,
}

/// One investigation opened: the record, its counts, and every member with its note.
///
/// `pins` is complete — `list_pins` pages to exhaustion, and this view carries no cursor because
/// there is nothing here that a caller could be given a prefix of without knowing. If an
/// investigation ever grows past what one response should carry, it gets a `Page` like every
/// profile section, rather than a silent cap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvestigationDetailView {
    pub investigation: SavedInvestigationView,
    pub entity_count: u64,
    pub edge_count: u64,
    pub pins: Vec<SavedPinView>,
}

/// `POST /api/investigations` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateInvestigationRequest {
    pub name: String,
    /// Investigation-level notes. Send `""` for none — no `#[serde(default)]`, per this crate's
    /// blanket rule, so a client that forgets the field is told rather than silently given one.
    pub notes: String,
}

/// `PATCH /api/investigation` body. Each field is *optional in meaning* — `None` means "leave this
/// alone" — but the **key is mandatory** (`required_option`), so a client that meant to clear the
/// notes and a client that forgot the field are distinguishable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateInvestigationRequest {
    #[serde(deserialize_with = "required_option")]
    pub name: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub notes: Option<String>,
}

/// `POST /api/investigation/pin` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinRequest {
    pub target: PinTargetView,
    pub origin: PinOriginView,
    /// `None` **preserves** an existing note rather than clearing it (45-07), so a re-pin from the
    /// canvas cannot erase what the analyst typed on the profile. `required_option`.
    #[serde(deserialize_with = "required_option")]
    pub note: Option<String>,
}

/// `POST /api/investigation/unpin` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnpinRequest {
    pub target: PinTargetView,
}

/// `PATCH /api/investigation/pin` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinNoteRequest {
    pub target: PinTargetView,
    pub note: String,
}

/// What plan 45-13's node cap had already done to the canvas **before** these filters ran.
///
/// Carried so a save can record it. A saved subgraph that silently dropped the cap would be the
/// defect CNV-03 exists to prevent, one level down and invisible: an investigation of 250 entities
/// looks identical whether 250 was the neighbourhood or the first 250 of 1,842.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpansionCutView {
    pub seed_id: String,
    pub hops: u32,
    pub node_cap: u32,
    /// How many nodes the walk reached before the cap.
    pub total_discovered: u64,
    /// How many came back.
    pub shown: u64,
    pub truncated: bool,
}

/// `POST /api/investigation/subgraph?id=…` body — CNV-03's "save the visible subgraph".
///
/// # Why the filter travels with the ids
///
/// So the server can **recompute** what was visible and refuse to save anything else. The client
/// sends what it believes is on screen (`expected_node_ids`) and the request that produced it; the
/// server re-runs the filter over the same scope and writes pins only if the two sets are equal.
///
/// That is a stronger guarantee than either half alone. Trusting the id list would let any client
/// bug become a saved investigation that never existed on a screen; recomputing without the
/// client's list would be exactly the defect this requirement names — a save that quietly re-runs
/// a query and saves whatever comes back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SaveSubgraphRequest {
    pub filter: crate::canvas_filter::CanvasFilterRequest,
    /// Every node the canvas says is visible. Compared as a **set**: the canvas orders by its own
    /// focus set and the server by the candidate order, and an ordering difference is not a
    /// disagreement about what was on screen.
    pub expected_node_ids: Vec<String>,
    /// `None` when no expansion was running.
    #[serde(deserialize_with = "required_option")]
    pub expansion_cut: Option<ExpansionCutView>,
}

/// What a subgraph save did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SaveSubgraphResultView {
    pub entity_count: u64,
    pub edge_count: u64,
    /// The sentence written into the investigation's notes, returned so the analyst reads the same
    /// words the investigation will carry rather than a cheerful summary of them.
    pub provenance_note: String,
}

/// What an unpin did. `false` means the member was not pinned — an honest idempotent no-op, and
/// the reason this is a body rather than a bare 204: the UI must not report "removed" for work
/// that did not happen.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnpinResultView {
    pub removed: bool,
}

/// What a delete did: how many members went with the investigation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteResultView {
    pub removed_members: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn investigation() -> SavedInvestigationView {
        SavedInvestigationView {
            id: "5f2b".to_string(),
            name: "Ward concentration follow-up".to_string(),
            analysis_db: "an_example".to_string(),
            notes: "".to_string(),
            schema_version: 1,
            created_at: "2026-09-15T10:00:00Z".to_string(),
            updated_at: "2026-09-15T11:00:00Z".to_string(),
        }
    }

    /// The tag values are the storage layer's own `kind()` strings, so the wire and the column
    /// share one vocabulary rather than needing a translation nobody maintains.
    #[test]
    fn a_pin_target_tags_itself_the_way_the_storage_column_does() {
        let entity = PinTargetView::Entity {
            entity_id: "GB-COH:X06999001".to_string(),
        };
        let edge = PinTargetView::Edge {
            from: "GB-COH:X06999001".to_string(),
            to: "contract:X9999-c1".to_string(),
            relation: "awarded_to".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&entity).expect("serialize")["kind"],
            "entity"
        );
        assert_eq!(
            serde_json::to_value(&edge).expect("serialize")["kind"],
            "edge"
        );

        let back: PinTargetView =
            serde_json::from_value(serde_json::to_value(&edge).expect("serialize"))
                .expect("deserialize");
        assert_eq!(back, edge);
    }

    /// A detail view round-trips with its members and their notes intact. Notes are analyst work
    /// product; a wire type that loses one is a wire type that loses work.
    #[test]
    fn an_investigation_round_trips_with_its_pins_and_notes() {
        let detail = InvestigationDetailView {
            investigation: investigation(),
            entity_count: 1,
            edge_count: 1,
            pins: vec![SavedPinView {
                id: "5f2b-entity".to_string(),
                target: PinTargetView::Entity {
                    entity_id: "GB-COH:X06999001".to_string(),
                },
                note: "same director as the ward's top supplier".to_string(),
                origin: PinOriginView::Profile,
                pinned_at: "2026-09-15T10:05:00Z".to_string(),
                updated_at: "2026-09-15T10:06:00Z".to_string(),
            }],
        };
        let json = serde_json::to_value(&detail).expect("serialize");
        assert_eq!(json["pins"][0]["origin"], "profile");
        let back: InvestigationDetailView = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, detail);
        assert_eq!(
            back.pins[0].note,
            "same director as the ward's top supplier"
        );
    }

    /// `note: None` on a pin means "keep what is there", and it must be a present `null` rather
    /// than an omitted key — the two mean different things to a re-pin.
    ///
    /// PROVE THIS CAN FAIL: removing `deserialize_with = "required_option"` from
    /// `PinRequest::note` makes the omitted-key assertion succeed in deserializing and the test
    /// fails. That mutation was run before this test was committed.
    #[test]
    fn a_pin_request_without_a_note_key_is_an_error_not_a_none() {
        let omitted = r#"{"target":{"kind":"entity","entity_id":"a"},"origin":"canvas"}"#;
        assert!(
            serde_json::from_str::<PinRequest>(omitted).is_err(),
            "an omitted `note` key must be an error: None means PRESERVE, and a client that \
             forgot the field has not asked for that"
        );

        let explicit =
            r#"{"target":{"kind":"entity","entity_id":"a"},"origin":"canvas","note":null}"#;
        let parsed: PinRequest = serde_json::from_str(explicit).expect("an explicit null parses");
        assert_eq!(parsed.note, None);
    }

    /// An update with neither field is representable on the wire and is the handler's to reject —
    /// the type does not silently turn it into a no-op that reports success.
    #[test]
    fn an_update_carries_both_keys_even_when_it_changes_one_thing() {
        let rename = UpdateInvestigationRequest {
            name: Some("New name".to_string()),
            notes: None,
        };
        let json = serde_json::to_string(&rename).expect("serialize");
        assert!(json.contains("\"notes\":null"), "{json}");
        assert!(serde_json::from_str::<UpdateInvestigationRequest>(r#"{"name":"x"}"#).is_err());
    }
}
