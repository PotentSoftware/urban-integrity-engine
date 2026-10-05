//! Saved investigations — the analyst's work product, and the only write path in this crate that
//! stores a real `datetime` (PIV-05, Phase 45 plan 07).
//!
//! # What this is for, and who else reads it
//!
//! PIV-05 asks for "a named saved investigation with free-text notes, persisted in the analysis
//! database", and then names three consumers that do not exist yet: **dossier export, scoped
//! export and subgraph extraction** (phases 47–48). So the shape here is settled by what an
//! export has to be able to reproduce, not by what this phase's pin button happens to need:
//!
//! * **Which analysis.** An analysis id is the *database*, not the namespace (docs/CONFIGURATION.md), and a
//!   row that travels out of its database into a dossier carries no statement of which analysis it
//!   describes. [`create_investigation`] therefore fills `analysis_db` from SurrealQL's own
//!   `session::db()` — never from a caller-supplied string, which could name a database the row is
//!   not in with nothing downstream able to tell.
//! * **When, and whence.** Every pin carries `pinned_at` and an [`PinOrigin`]: an entity the
//!   analyst pinned by hand off a profile is a different claim from one that arrived inside a bulk
//!   subgraph save, and a set that cannot tell them apart reads every member as deliberate.
//!   Re-pinning preserves both `pinned_at` and the note, so an idempotent re-pin cannot quietly
//!   restate when the analyst found something.
//! * **Edges, not only nodes.** CNV-03/plan 45-14 saves "exactly what was visible" on the canvas,
//!   and the visible edge set is *not* recoverable from the node set: the canvas shows an
//!   edge-type-filtered subset of the induced edges, so re-deriving at export time would silently
//!   restore edges the analyst had filtered out. Hence [`PinTarget::Edge`].
//!
//! What is deliberately **not** stored: a copy of the pinned facts. An investigation stores
//! *references*. A dossier must reflect the data as it stands when it is exported, and a frozen
//! copy here would let an export contradict the live profile with no way to tell which was right —
//! provenance (`workbench_dto::provenance`) is a property of a read, recomputed at read time, not
//! something to denormalise into a pin. Nor is the as-at date or the canvas filter combination
//! stored: 45-06 fixed that an as-at filter *marks* rows and removes none, so it is a display
//! setting, and 45-14's requirement is satisfied by writing the resulting member set — the members
//! are the content, the filter that produced them is not.
//!
//! # The write idiom, and the two incidents that fix it
//!
//! Every write here is all-`SET` with bound parameters, `.check()?` on the response and
//! `.take(0)?` on the rows, and every one of them checks that a row actually came back. Copied
//! from `crates/ingestor/src/backfill/anomaly_sink.rs`, the canonical fail-loud write in this
//! workspace, because the surrounding `crates/core::persistence` modules cannot be copied: they
//! store int milliseconds (`detector_runs`, `land_flip_runs`) and so never meet the rule this
//! module has to.
//!
//! 1. **SurrealDB 3.x will not coerce a string into a `datetime` field.** `CONTENT` plus a bare
//!    `.await?` drops the row *silently*; the PPD enrichment reported "2795151 written" against 0
//!    staged rows and every unit test was green. Datetimes here are bound as chrono
//!    [`DateTime<Utc>`] values, which the driver serialises as real datetimes.
//! 2. **Never discard a write `Result`.** The ICIJ adapter wrote 0 rows for its entire life behind
//!    `let _ = db.query(...)` while logging 814k writes. There is no `let _` in this file, and a
//!    write that matches nothing is an `Err` rather than a quiet `Ok(())` — that matters most for
//!    the note-editing paths, where "saved" appearing on screen over a write that hit no row would
//!    lose the analyst's own words.
//!
//! Two consequences worth stating, because they look like over-caution until they are not:
//!
//! * A pin's `investigation` is resolved inside the statement, by `array::first(SELECT VALUE id
//!   FROM …)`. A missing investigation makes that `NONE`, and the `record<saved_investigation>`
//!   column refuses it: `Expected 'record<saved_investigation>' but found NONE`. So a pin can
//!   never be written against an investigation that is not there, atomically and without a
//!   read-then-write race.
//! * A bound Rust `None` arrives as SurrealQL `NONE`. An `option<string>` column takes that
//!   happily — measured, by removing the guard and watching the edge pin still round-trip — so the
//!   four optional member columns bind directly. `note` is a plain `string` column and refuses
//!   `NONE` (`Expected 'string' but found 'NONE'`), which is exactly what makes
//!   `note = IF type::is_string($note) THEN $note ELSE (IF type::is_string(note) THEN note ELSE ''
//!   END) END` the whole mechanism for "a re-pin with no note leaves the analyst's note alone":
//!   one statement that means both "set this" and "keep what is there".
//!
//! # Running the tests
//!
//! ```text
//! URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test saved_investigations
//! ```

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use surrealdb::{engine::any::Any, Surreal};

use super::scan::SCAN_MAX_ROWS;

/// The investigation table. One row per named set.
pub const INVESTIGATION_TABLE: &str = "saved_investigation";

/// The member table. One row per pinned entity or edge.
pub const PIN_TABLE: &str = "saved_investigation_pin";

/// Stamped on every investigation, so a phase-47 reader can refuse an older shape instead of
/// reading an absent field as a default — which is how "0 pins" and "pins this reader cannot see"
/// become indistinguishable. Bump it only when the stored shape changes meaning.
pub const SAVED_INVESTIGATION_SCHEMA_VERSION: i64 = 1;

/// Rows per page when reading an investigation's members.
///
/// An investigation is a hand-curated set for a person, but 45-14 writes a whole visible subgraph
/// into one in a single action, so "it will always be small" is not a safe assumption to page on.
const PIN_PAGE_SIZE: usize = 5_000;

// ── Types ────────────────────────────────────────────────────────────────────

/// What a pin points at.
///
/// Two variants, not one id column with a convention: an edge is three strings and a direction,
/// and squeezing it into an entity id would make the distinction depend on how a reader parsed a
/// string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinTarget {
    /// A graph node, by its `EntityId` string (`director:jane-doe`, `GB-COH:12345678`, …).
    Entity { entity_id: String },
    /// A directed edge between two nodes, as it was visible on the canvas. `relation` is the edge
    /// type (`directs`, `awarded_to`, …).
    Edge {
        from: String,
        to: String,
        relation: String,
    },
}

impl PinTarget {
    /// The `kind` column: the discriminator a reader decodes on.
    pub fn kind(&self) -> &'static str {
        match self {
            PinTarget::Entity { .. } => "entity",
            PinTarget::Edge { .. } => "edge",
        }
    }

    /// The deterministic natural key. The record id is built from it, which is what makes pinning
    /// idempotent — "pinning the same entity twice does not create two entries" (PIV-05).
    ///
    /// An edge's key carries its direction. `A -> B` and `B -> A` are two different facts and must
    /// not collapse into one pin.
    pub fn member_key(&self) -> String {
        match self {
            PinTarget::Entity { entity_id } => format!("entity:{entity_id}"),
            PinTarget::Edge { from, to, relation } => format!("edge:{relation}:{from}->{to}"),
        }
    }

    /// Reject a target that could not identify anything.
    ///
    /// An empty entity id would pin "nothing" under a well-formed key, and the set would then
    /// carry a member no export could resolve.
    fn validate(&self) -> Result<()> {
        let empty = match self {
            PinTarget::Entity { entity_id } => entity_id.trim().is_empty(),
            PinTarget::Edge { from, to, relation } => {
                from.trim().is_empty() || to.trim().is_empty() || relation.trim().is_empty()
            }
        };
        if empty {
            bail!("a pin target must name something: {self:?} has an empty component");
        }
        Ok(())
    }

    /// Reconstruct a target from a stored row.
    ///
    /// A row whose `kind` disagrees with the columns that are populated is an **error**, not a
    /// best guess: silently defaulting here is how a half-written row becomes a plausible-looking
    /// member of someone's dossier.
    fn from_row(row: &Value) -> Result<Self> {
        let kind = str_field(row, "kind")?;
        match kind.as_str() {
            "entity" => Ok(PinTarget::Entity {
                entity_id: str_field(row, "entity_id")?,
            }),
            "edge" => Ok(PinTarget::Edge {
                from: str_field(row, "edge_from")?,
                to: str_field(row, "edge_to")?,
                relation: str_field(row, "edge_relation")?,
            }),
            other => bail!("unknown saved_investigation_pin kind {other:?}"),
        }
    }
}

/// Which surface a member entered the set from.
///
/// Parsed strictly: an unknown value is an error rather than a default. Adding a variant is a
/// deliberate act, and mapping an unrecognised origin onto an existing one would have a dossier
/// attribute a member to a surface it did not come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOrigin {
    /// Pinned by hand from an entity profile (`/entity/:id`).
    Profile,
    /// Pinned by hand from the canvas inspection panel (`/graph`).
    Canvas,
    /// Arrived as part of a bulk "save visible subgraph" (CNV-03, plan 45-14).
    Subgraph,
    /// Pinned by hand from the alert inbox (`/alerts`, `/alert/:id`) — the alert's own
    /// primary/hub participant, Phase 46 D-08.
    Alert,
}

impl PinOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            PinOrigin::Profile => "profile",
            PinOrigin::Canvas => "canvas",
            PinOrigin::Subgraph => "subgraph",
            PinOrigin::Alert => "alert",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "profile" => Ok(PinOrigin::Profile),
            "canvas" => Ok(PinOrigin::Canvas),
            "subgraph" => Ok(PinOrigin::Subgraph),
            "alert" => Ok(PinOrigin::Alert),
            other => bail!("unknown saved_investigation_pin origin {other:?}"),
        }
    }
}

/// One saved investigation, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedInvestigation {
    /// The record key, without the table prefix.
    pub id: String,
    pub name: String,
    /// The analysis database this belongs to, as the server saw it at write time.
    pub analysis_db: String,
    pub notes: String,
    pub schema_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One pinned member, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPin {
    /// The record key, without the table prefix.
    pub id: String,
    /// The owning investigation's record key, without the table prefix.
    pub investigation_id: String,
    pub target: PinTarget,
    pub note: String,
    pub origin: PinOrigin,
    /// When the member FIRST entered the set — preserved across re-pins.
    pub pinned_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An investigation plus the counts the list view shows.
///
/// Entities and edges are counted separately because they are not the same thing to an analyst:
/// "14 entities" is a claim about how many subjects are in the file, and a subgraph save that
/// brought 400 edges with it must not inflate that number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationSummary {
    pub investigation: SavedInvestigation,
    pub entity_count: usize,
    pub edge_count: usize,
}

// ── Investigations ───────────────────────────────────────────────────────────

/// Create a named investigation in the database the connection is pointed at.
///
/// `analysis_db` is filled by the server from `session::db()`, so the stored analysis and the
/// database the row landed in cannot disagree.
pub async fn create_investigation(
    db: &Surreal<Any>,
    name: &str,
    notes: &str,
) -> Result<SavedInvestigation> {
    let name = validated_name(name)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let now = Utc::now();

    let rows: Vec<Value> = db
        .query(
            "CREATE type::record($tb, $id) SET \
                 name = $name, \
                 analysis_db = session::db(), \
                 notes = $notes, \
                 schema_version = $schema_version, \
                 created_at = $now, \
                 updated_at = $now",
        )
        .bind(("tb", INVESTIGATION_TABLE))
        .bind(("id", id.clone()))
        .bind(("name", name))
        .bind(("notes", notes.to_string()))
        .bind(("schema_version", SAVED_INVESTIGATION_SCHEMA_VERSION))
        .bind(("now", now))
        .await?
        .check()?
        .take(0)?;

    let row = rows.into_iter().next().with_context(|| {
        format!("create_investigation wrote no row for {id} — the investigation was lost")
    })?;
    parse_investigation(&row)
}

/// Read one investigation back, or `None` if there is no such record.
pub async fn get_investigation(db: &Surreal<Any>, id: &str) -> Result<Option<SavedInvestigation>> {
    let rows: Vec<Value> = db
        .query("SELECT * FROM type::record($tb, $id)")
        .bind(("tb", INVESTIGATION_TABLE))
        .bind(("id", id.to_string()))
        .await?
        .check()?
        .take(0)?;
    rows.first().map(parse_investigation).transpose()
}

/// Every investigation in this analysis, most recently modified first, with its member counts.
///
/// Paged by `id` and re-sorted in Rust rather than paged by `updated_at`: `ORDER BY id` with
/// `START` advanced by rows *actually received* is this workspace's completeness rule
/// (`persistence::scan`), and a page ordered by a column the writer mutates can drop or repeat
/// rows between pages.
pub async fn list_investigations(db: &Surreal<Any>) -> Result<Vec<InvestigationSummary>> {
    let mut investigations: Vec<SavedInvestigation> = Vec::new();
    let mut start = 0usize;
    loop {
        let rows: Vec<Value> = db
            .query(format!(
                "SELECT * FROM {INVESTIGATION_TABLE} ORDER BY id LIMIT {PIN_PAGE_SIZE} START {start}"
            ))
            .await?
            .check()?
            .take(0)?;
        if rows.is_empty() {
            break;
        }
        start += rows.len();
        for row in &rows {
            investigations.push(parse_investigation(row)?);
        }
        if start >= SCAN_MAX_ROWS {
            bail!("list_investigations hit the {SCAN_MAX_ROWS}-row scan cap — refusing to report a truncated list as complete");
        }
    }

    // One grouped count for the whole table beats one count per investigation: the list view is
    // the first thing the shell navigation opens, and N+1 round trips there is a page that gets
    // slower the more work the analyst has done.
    let counts: Vec<Value> = db
        .query(format!(
            "SELECT investigation, kind, count() AS n FROM {PIN_TABLE} GROUP BY investigation, kind"
        ))
        .await?
        .check()?
        .take(0)?;

    let mut summaries: Vec<InvestigationSummary> = investigations
        .into_iter()
        .map(|investigation| InvestigationSummary {
            investigation,
            entity_count: 0,
            edge_count: 0,
        })
        .collect();

    for row in &counts {
        let investigation = record_key(&str_field(row, "investigation")?);
        let kind = str_field(row, "kind")?;
        let n =
            row.get("n").and_then(Value::as_i64).with_context(|| {
                format!("pin count row for {investigation} has no readable count")
            })? as usize;
        let Some(summary) = summaries
            .iter_mut()
            .find(|s| s.investigation.id == investigation)
        else {
            // A pin whose investigation is gone. It cannot happen through this module
            // (`delete_investigation` removes the members first), so it means someone deleted a
            // row by hand — worth a line in the log, not worth failing an analyst's list page.
            tracing::warn!(
                investigation = %investigation,
                "saved_investigation_pin rows reference an investigation that no longer exists"
            );
            continue;
        };
        match kind.as_str() {
            "entity" => summary.entity_count = n,
            "edge" => summary.edge_count = n,
            other => bail!("unknown saved_investigation_pin kind {other:?} in count rows"),
        }
    }

    summaries.sort_by(|a, b| {
        b.investigation
            .updated_at
            .cmp(&a.investigation.updated_at)
            .then_with(|| a.investigation.id.cmp(&b.investigation.id))
    });
    Ok(summaries)
}

/// Rename an investigation. Errors if there is no such record — a rename that hit nothing must not
/// return `Ok`.
pub async fn rename_investigation(
    db: &Surreal<Any>,
    id: &str,
    name: &str,
) -> Result<SavedInvestigation> {
    let name = validated_name(name)?;
    update_investigation_field(db, id, "name = $value", name, "rename_investigation").await
}

/// Replace an investigation's notes.
///
/// Errors if there is no such record. Notes are analyst work product: a save that matched no row
/// and still reported success would put "Saved" on screen over words that are gone.
pub async fn set_investigation_notes(
    db: &Surreal<Any>,
    id: &str,
    notes: &str,
) -> Result<SavedInvestigation> {
    update_investigation_field(
        db,
        id,
        "notes = $value",
        notes.to_string(),
        "set_investigation_notes",
    )
    .await
}

async fn update_investigation_field(
    db: &Surreal<Any>,
    id: &str,
    assignment: &str,
    value: String,
    caller: &str,
) -> Result<SavedInvestigation> {
    let rows: Vec<Value> = db
        .query(format!(
            "UPDATE type::record($tb, $id) SET {assignment}, updated_at = $now"
        ))
        .bind(("tb", INVESTIGATION_TABLE))
        .bind(("id", id.to_string()))
        .bind(("value", value))
        .bind(("now", Utc::now()))
        .await?
        .check()?
        .take(0)?;

    let row = rows.into_iter().next().with_context(|| {
        format!("{caller} matched no investigation {id} — nothing was saved, and the caller must not report success")
    })?;
    parse_investigation(&row)
}

/// Delete an investigation and every member of it.
///
/// Returns the number of members removed, or `None` when there was no such investigation.
///
/// Members go first on purpose. If the second statement then fails the analyst is left with a
/// visible, empty investigation they can delete again; the other order would leave invisible
/// orphan members pointing at a record that is gone.
pub async fn delete_investigation(db: &Surreal<Any>, id: &str) -> Result<Option<usize>> {
    let removed: Vec<Value> = db
        .query(format!(
            "DELETE {PIN_TABLE} WHERE investigation = type::record($tb, $id) RETURN BEFORE"
        ))
        .bind(("tb", INVESTIGATION_TABLE))
        .bind(("id", id.to_string()))
        .await?
        .check()?
        .take(0)?;

    let gone: Vec<Value> = db
        .query("DELETE type::record($tb, $id) RETURN BEFORE")
        .bind(("tb", INVESTIGATION_TABLE))
        .bind(("id", id.to_string()))
        .await?
        .check()?
        .take(0)?;

    if gone.is_empty() {
        if !removed.is_empty() {
            // Members existed without their owner — see the warning in `list_investigations`.
            tracing::warn!(
                investigation = %id,
                removed = removed.len(),
                "deleted saved_investigation_pin rows whose investigation record was already gone"
            );
        }
        return Ok(None);
    }
    Ok(Some(removed.len()))
}

// ── Members ──────────────────────────────────────────────────────────────────

/// Pin a member into an investigation. Idempotent: pinning the same member twice updates one row.
///
/// `note = None` **preserves** an existing note rather than clearing it, so a re-pin from the
/// canvas cannot erase what the analyst typed on the profile. `pinned_at` is likewise preserved,
/// so the set keeps saying when the member was first found.
///
/// Fails if the investigation does not exist: the `record<saved_investigation>` column refuses the
/// `NONE` that the id lookup yields, inside the same statement, so no dangling member is written.
pub async fn pin_member(
    db: &Surreal<Any>,
    investigation_id: &str,
    target: &PinTarget,
    origin: PinOrigin,
    note: Option<&str>,
) -> Result<SavedPin> {
    target.validate()?;
    let member_key = target.member_key();
    let pin_id = pin_record_key(investigation_id, &member_key);
    let now = Utc::now();

    let (entity_id, edge_from, edge_to, edge_relation) = match target {
        PinTarget::Entity { entity_id } => (Some(entity_id.clone()), None, None, None),
        PinTarget::Edge { from, to, relation } => (
            None,
            Some(from.clone()),
            Some(to.clone()),
            Some(relation.clone()),
        ),
    };

    let mut response = db
        .query(
            "LET $inv = array::first(SELECT VALUE id FROM type::record($inv_tb, $inv_id)); \
             UPSERT type::record($pin_tb, $pin_id) SET \
                 investigation = $inv, \
                 kind = $kind, \
                 member_key = $member_key, \
                 entity_id = $entity_id, \
                 edge_from = $edge_from, \
                 edge_to = $edge_to, \
                 edge_relation = $edge_relation, \
                 origin = $origin, \
                 note = IF type::is_string($note) THEN $note ELSE (IF type::is_string(note) THEN note ELSE '' END) END, \
                 pinned_at = IF type::is_datetime(pinned_at) THEN pinned_at ELSE $now END, \
                 updated_at = $now; \
             UPDATE type::record($inv_tb, $inv_id) SET updated_at = $now",
        )
        .bind(("inv_tb", INVESTIGATION_TABLE))
        .bind(("inv_id", investigation_id.to_string()))
        .bind(("pin_tb", PIN_TABLE))
        .bind(("pin_id", pin_id.clone()))
        .bind(("kind", target.kind()))
        .bind(("member_key", member_key.clone()))
        .bind(("entity_id", entity_id))
        .bind(("edge_from", edge_from))
        .bind(("edge_to", edge_to))
        .bind(("edge_relation", edge_relation))
        .bind(("origin", origin.as_str()))
        .bind(("note", note.map(str::to_string)))
        .bind(("now", now))
        .await?
        .check()?;

    // Statement 1 is the pin; statement 2 is the investigation's `updated_at`. Both are read, so
    // neither can fail unnoticed.
    let pinned: Vec<Value> = response.take(1)?;
    let touched: Vec<Value> = response.take(2)?;

    let row = pinned
        .into_iter()
        .next()
        .with_context(|| format!("pin_member wrote no row for {pin_id} — the pin was lost"))?;
    if touched.is_empty() {
        bail!("pin_member wrote {pin_id} but did not touch investigation {investigation_id} — its last-modified time is now wrong");
    }
    parse_pin(&row)
}

/// Remove a member. Returns `false` when it was not pinned in the first place.
///
/// The investigation's `updated_at` moves only when something was actually removed, so the list's
/// last-modified column never reports work that did not happen.
pub async fn unpin_member(
    db: &Surreal<Any>,
    investigation_id: &str,
    target: &PinTarget,
) -> Result<bool> {
    target.validate()?;
    let pin_id = pin_record_key(investigation_id, &target.member_key());

    let removed: Vec<Value> = db
        .query("DELETE type::record($tb, $id) RETURN BEFORE")
        .bind(("tb", PIN_TABLE))
        .bind(("id", pin_id))
        .await?
        .check()?
        .take(0)?;
    if removed.is_empty() {
        return Ok(false);
    }

    let touched: Vec<Value> = db
        .query("UPDATE type::record($tb, $id) SET updated_at = $now")
        .bind(("tb", INVESTIGATION_TABLE))
        .bind(("id", investigation_id.to_string()))
        .bind(("now", Utc::now()))
        .await?
        .check()?
        .take(0)?;
    if touched.is_empty() {
        bail!("unpin_member removed a member of {investigation_id} but the investigation record is gone");
    }
    Ok(true)
}

/// Replace one member's note. Errors when the member is not pinned — see
/// [`set_investigation_notes`] for why a no-op must not look like a save.
pub async fn set_pin_note(
    db: &Surreal<Any>,
    investigation_id: &str,
    target: &PinTarget,
    note: &str,
) -> Result<SavedPin> {
    target.validate()?;
    let pin_id = pin_record_key(investigation_id, &target.member_key());
    let now = Utc::now();

    let mut response = db
        .query(
            "UPDATE type::record($pin_tb, $pin_id) SET note = $note, updated_at = $now; \
             UPDATE type::record($inv_tb, $inv_id) SET updated_at = $now",
        )
        .bind(("pin_tb", PIN_TABLE))
        .bind(("pin_id", pin_id.clone()))
        .bind(("inv_tb", INVESTIGATION_TABLE))
        .bind(("inv_id", investigation_id.to_string()))
        .bind(("note", note.to_string()))
        .bind(("now", now))
        .await?
        .check()?;

    let updated: Vec<Value> = response.take(0)?;
    let touched: Vec<Value> = response.take(1)?;

    let row = updated.into_iter().next().with_context(|| {
        format!("set_pin_note matched no pin {pin_id} — the note was not saved, and the caller must not report success")
    })?;
    if touched.is_empty() {
        bail!("set_pin_note saved a note on {pin_id} but investigation {investigation_id} is gone");
    }
    parse_pin(&row)
}

/// Every member of an investigation, oldest pin first.
///
/// Paged by `id` with `START` advanced by rows actually received, terminating only on an empty
/// page — a short page means the response was truncated, not that the data ended
/// (`persistence::scan`, rule 2).
pub async fn list_pins(db: &Surreal<Any>, investigation_id: &str) -> Result<Vec<SavedPin>> {
    let mut pins = Vec::new();
    let mut start = 0usize;
    loop {
        let rows: Vec<Value> = db
            .query(format!(
                "SELECT * FROM {PIN_TABLE} WHERE investigation = type::record($tb, $id) \
                 ORDER BY id LIMIT {PIN_PAGE_SIZE} START {start}"
            ))
            .bind(("tb", INVESTIGATION_TABLE))
            .bind(("id", investigation_id.to_string()))
            .await?
            .check()?
            .take(0)?;
        if rows.is_empty() {
            break;
        }
        start += rows.len();
        for row in &rows {
            pins.push(parse_pin(row)?);
        }
        if start >= SCAN_MAX_ROWS {
            bail!("list_pins hit the {SCAN_MAX_ROWS}-row scan cap for {investigation_id} — refusing to report a truncated set as complete");
        }
    }
    pins.sort_by(|a, b| a.pinned_at.cmp(&b.pinned_at).then_with(|| a.id.cmp(&b.id)));
    Ok(pins)
}

// ── Keys, parsing ────────────────────────────────────────────────────────────

/// The pin's record key: the investigation's key, then the member's natural key.
///
/// Deterministic, so the `UPSERT` that writes it is the idempotency mechanism rather than a
/// separate dedup query. Investigation ids are simple UUIDs (32 hex characters, no separator), so
/// the `__` join is unambiguous.
pub fn pin_record_key(investigation_id: &str, member_key: &str) -> String {
    format!("{investigation_id}__{member_key}")
}

/// A name that would be unusable in the investigation list is refused at the door.
fn validated_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("an investigation needs a name — an unnamed row cannot be told apart in the list");
    }
    Ok(trimmed.to_string())
}

/// `saved_investigation:abc123` → `abc123`. Record ids arrive with their table prefix, and every
/// id this module hands back or looks up is the bare key.
fn record_key(id: &str) -> String {
    id.split_once(':')
        .map(|(_, key)| key)
        .unwrap_or(id)
        .trim_matches('`')
        .to_string()
}

fn str_field(row: &Value, field: &str) -> Result<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("row is missing the {field} field, or it is not a string: {row}"))
}

/// Parse a stored timestamp back into chrono.
///
/// The column is a real `datetime`; the driver renders it into `serde_json` as an RFC3339 string,
/// so this is the inverse of the write, not evidence about what the column holds. The column type
/// is asserted separately, in the database, by `crates/core/tests/saved_investigations.rs`.
fn datetime_field(row: &Value, field: &str) -> Result<DateTime<Utc>> {
    let raw = str_field(row, field)?;
    Ok(DateTime::parse_from_rfc3339(&raw)
        .with_context(|| format!("{field} is not an RFC3339 timestamp: {raw:?}"))?
        .with_timezone(&Utc))
}

fn parse_investigation(row: &Value) -> Result<SavedInvestigation> {
    Ok(SavedInvestigation {
        id: record_key(&str_field(row, "id")?),
        name: str_field(row, "name")?,
        analysis_db: str_field(row, "analysis_db")?,
        notes: str_field(row, "notes")?,
        schema_version: row
            .get("schema_version")
            .and_then(Value::as_i64)
            .with_context(|| format!("investigation row has no readable schema_version: {row}"))?,
        created_at: datetime_field(row, "created_at")?,
        updated_at: datetime_field(row, "updated_at")?,
    })
}

fn parse_pin(row: &Value) -> Result<SavedPin> {
    Ok(SavedPin {
        id: record_key(&str_field(row, "id")?),
        investigation_id: record_key(&str_field(row, "investigation")?),
        target: PinTarget::from_row(row)?,
        note: str_field(row, "note")?,
        origin: PinOrigin::parse(&str_field(row, "origin")?)?,
        pinned_at: datetime_field(row, "pinned_at")?,
        updated_at: datetime_field(row, "updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_member_key_is_stable_and_an_edge_carries_its_direction() {
        let a = PinTarget::Entity {
            entity_id: "director:jane-doe".into(),
        };
        assert_eq!(a.member_key(), "entity:director:jane-doe");
        assert_eq!(a.member_key(), a.clone().member_key());

        let forward = PinTarget::Edge {
            from: "GB-COH:1".into(),
            to: "director:jane-doe".into(),
            relation: "directs".into(),
        };
        let backward = PinTarget::Edge {
            from: "director:jane-doe".into(),
            to: "GB-COH:1".into(),
            relation: "directs".into(),
        };
        assert_ne!(
            forward.member_key(),
            backward.member_key(),
            "A->B and B->A are two different facts and must not collapse into one pin"
        );
    }

    /// Two members of one investigation get two record ids; one member of two investigations does
    /// too. Either collision would silently merge two analysts' sets — or two facts.
    #[test]
    fn pin_record_keys_do_not_collide() {
        let one = PinTarget::Entity {
            entity_id: "GB-COH:1".into(),
        };
        let two = PinTarget::Entity {
            entity_id: "GB-COH:2".into(),
        };
        assert_ne!(
            pin_record_key("inv1", &one.member_key()),
            pin_record_key("inv1", &two.member_key())
        );
        assert_ne!(
            pin_record_key("inv1", &one.member_key()),
            pin_record_key("inv2", &one.member_key())
        );
        assert_eq!(
            pin_record_key("inv1", &one.member_key()),
            pin_record_key("inv1", &one.member_key()),
            "the same member in the same investigation must produce the same record id — this is \
             what makes pinning idempotent"
        );
    }

    #[test]
    fn a_target_that_names_nothing_is_refused() {
        assert!(PinTarget::Entity {
            entity_id: "   ".into()
        }
        .validate()
        .is_err());
        assert!(PinTarget::Edge {
            from: "a".into(),
            to: "b".into(),
            relation: "".into()
        }
        .validate()
        .is_err());
        assert!(PinTarget::Entity {
            entity_id: "GB-COH:1".into()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn an_unnamed_investigation_is_refused_and_a_name_is_trimmed() {
        assert!(validated_name("").is_err());
        assert!(validated_name("   \n").is_err());
        assert_eq!(
            validated_name("  Anglesey land  ").unwrap(),
            "Anglesey land"
        );
    }

    /// A row whose `kind` disagrees with its populated columns must not decode into a plausible
    /// member. This is the shape a half-written row takes.
    #[test]
    fn a_pin_row_whose_kind_disagrees_with_its_columns_fails_to_decode() {
        let entity_kind_edge_columns = json!({
            "kind": "entity",
            "edge_from": "a", "edge_to": "b", "edge_relation": "directs"
        });
        assert!(PinTarget::from_row(&entity_kind_edge_columns).is_err());

        let edge_kind_entity_column = json!({ "kind": "edge", "entity_id": "GB-COH:1" });
        assert!(PinTarget::from_row(&edge_kind_entity_column).is_err());

        let unknown = json!({ "kind": "hunch", "entity_id": "GB-COH:1" });
        assert!(PinTarget::from_row(&unknown).is_err());
    }

    #[test]
    fn an_unknown_origin_is_an_error_not_a_default() {
        assert!(PinOrigin::parse("subgraph").is_ok());
        assert!(PinOrigin::parse("Subgraph").is_err());
        assert!(PinOrigin::parse("").is_err());
        for origin in [
            PinOrigin::Profile,
            PinOrigin::Canvas,
            PinOrigin::Subgraph,
            PinOrigin::Alert,
        ] {
            assert_eq!(PinOrigin::parse(origin.as_str()).unwrap(), origin);
        }
    }

    /// **A stored row written before `PinOrigin::Alert` existed must still parse unchanged.**
    ///
    /// `parse_pin` reads a raw `graph_events`/`saved_investigation_pin` row exactly as
    /// `crates/core::persistence::queries` phase 50's `ProbeView.motif_kind` incident found: a
    /// no-default field added to a persisted shape can make every stored row of the OLD shape
    /// undeserializable. `PinOrigin` is not a new field, but it IS a persisted, no-default,
    /// exhaustively-matched enum, and adding a 4th variant is exactly the shape of edit that
    /// incident warns about — so this fixture is dated at the moment of the edit and proves the
    /// three pre-existing origins still round-trip through `parse_pin` afterwards.
    ///
    /// PROVE THIS CAN FAIL: change `PinOrigin::parse`'s `"profile" => Ok(PinOrigin::Profile)` arm
    /// to require a different literal and this fails on the `Profile` fixture row.
    #[test]
    fn parse_pin_accepts_rows_written_before_the_alert_origin_existed() {
        fn fixture_row(origin: &str) -> Value {
            json!({
                "id": "saved_investigation_pin:abc__entity:GB-COH:99000001",
                "investigation": "saved_investigation:abc",
                "kind": "entity",
                "entity_id": "GB-COH:99000001",
                "note": "",
                "origin": origin,
                "pinned_at": "2026-06-01T00:00:00Z",
                "updated_at": "2026-06-01T00:00:00Z",
            })
        }

        for (origin_str, expected) in [
            ("profile", PinOrigin::Profile),
            ("canvas", PinOrigin::Canvas),
            ("subgraph", PinOrigin::Subgraph),
        ] {
            let row = fixture_row(origin_str);
            let pin = parse_pin(&row).unwrap_or_else(|e| {
                panic!("pre-existing origin {origin_str:?} must still parse: {e}")
            });
            assert_eq!(
                pin.origin, expected,
                "a pre-existing row's origin must round-trip to the same variant"
            );
        }
    }

    #[test]
    fn a_record_id_loses_its_table_prefix_and_its_backticks() {
        assert_eq!(record_key("saved_investigation:abc123"), "abc123");
        assert_eq!(
            record_key("saved_investigation_pin:`abc__entity:GB-COH:1`"),
            "abc__entity:GB-COH:1"
        );
        assert_eq!(record_key("abc123"), "abc123");
    }
}
