//! `GraphPage` — the `/graph` route. Beneath the population-metadata panel (unchanged from
//! Phase 43), this plan (44-10) mounts the interactive Cosmograph canvas from the population's
//! Arrow IPC export, once, outside Leptos's reactive path (D-11/D-17/CNV-04), plus the node-click
//! side card (D-12). The tool rail, results list and status line arrive in later Phase 44 plans —
//! this page's canvas/side-card half is independently verifiable now.
//!
//! # Testing approach (plain-function-first, per `status_page.rs`'s convention)
//!
//! No headless-browser runner is wired into this workspace's `cargo test` (the phase 42 research notes
//! Pitfall 10), so the fetch URLs, the exact error copy, the "never a fabricated 0" rule, and the
//! hand-rolled Arrow IPC byte-framing/column-extraction logic (see the module doc comment on
//! [`parse_utf8_column`] for why this crate cannot depend on the `arrow` crate) are pulled out as
//! plain constants/functions and tested directly, independent of any wasm/JS runtime.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use urban_integrity_workbench_dto::canvas_filter::MissingEvidence;
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;
use urban_integrity_workbench_dto::{
    AlgorithmOutcome, AlgorithmResult, EntityRef, PopulationMeta, StatusView,
};
use wasm_bindgen::JsCast;

use crate::breadcrumb::Breadcrumb;
use crate::canvas_filters::{self, CanvasFilterSignals, CanvasFilters, SaveVisibleSubgraph};
use crate::cosmograph_trait::CosmographHandle;
use crate::edge_palette;
use crate::entity_picker;
use crate::graph_deep_link;
use crate::graph_results_list::ResultsList;
use crate::graph_status_line::{
    automatic_effect_for, betweenness_resize_scale, CanvasEffect, StatusLine,
};
use crate::neighbourhood::{self, ClusterGlyphs, HopControl};
use crate::node_palette;
use crate::side_card::SideCard;
use crate::timestamp::Timestamp;
use crate::tool_rail::{self, ToolRail, ToolRailSignals};

/// `GET /api/graph/population/meta` — the native endpoint plan 43-05 built
/// (`population_meta_handler`).
pub const POPULATION_META_URL: &str = "/api/graph/population/meta";

/// `GET /api/workbench-status` — the same native endpoint `status_page.rs` (Phase 42) used before
/// Plan 09 deleted it. Defined locally here (not imported) since Plan 09 removed `status_page.rs`
/// entirely once its one real screen was superseded by the route table's 5 real pages — this
/// page is now the sole consumer of the endpoint for node/edge counts.
pub const STATUS_URL: &str = "/api/workbench-status";

/// The exact D-23 error copy, with `/api/graph/population/meta`'s literal endpoint path
/// substituted — shown verbatim on a fetch/deserialization failure of EITHER backing endpoint,
/// never a generic/paraphrased message and never partial data.
pub const GRAPH_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/graph/population/meta).";

/// Caption for the ward-seed-count figure in the population panel (checkpoint fix item 1,
/// 43-09 Task 3 review): rendered in the same value+caption pattern as the nodes/edges pair —
/// never a bare, unlabeled number.
pub const WARD_SEED_CAPTION: &str = "ward seeds";

/// The honest heading shown while a large replay's CSR bake has not yet landed (checkpoint fix
/// item 4) — never a fabricated `0` node/edge count in that window.
pub const SNAPSHOT_BAKING_HEADING: &str = "Graph snapshot baking\u{2026}";

/// `GET /api/graph/population` — the native endpoint plan 44-08 built (`population_handler`),
/// returning the population-scoped Arrow IPC export (plan 44-06's `build_arrow_ipc`).
pub const POPULATION_ARROW_URL: &str = "/api/graph/population";

/// The D-23 / phase 44 UI spec "Error state — population load failure" copy, verbatim, shown with a
/// Retry button when [`POPULATION_ARROW_URL`] cannot be fetched or its bytes cannot be split into
/// the nodes/links streams [`split_population_arrow`] expects — the canvas is not mounted in
/// either case. A `503` is NOT one of these cases: see [`POPULATION_ARROW_BAKING_COPY`].
pub const POPULATION_ARROW_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/graph/population).";

/// Shown instead of [`POPULATION_ARROW_FETCH_ERROR_COPY`] when the population endpoint answers
/// `503` — which `population_handler` does, deliberately and honestly, for as long as the CSR
/// bake has not landed (~5 minutes on a large live corpus). That is a *wait longer* condition,
/// not unreadable data, and reporting it as the latter sent the operator hunting a corrupt
/// payload on 2026-09-13 when the payload was fine. No Retry button accompanies this state: the
/// page retries itself every [`POPULATION_BAKING_RETRY`].
pub const POPULATION_ARROW_BAKING_COPY: &str =
    "Graph snapshot baking\u{2026} the canvas appears on its own once it lands.";

/// How long the page waits before re-requesting a population that answered `503`. Matches the
/// status panel's own baking refetch interval, so the two states clear at roughly the same time.
const POPULATION_BAKING_RETRY: Duration = Duration::from_secs(5);

/// Why [`fetch_population_arrow`] failed. The distinction exists because the two cases call for
/// opposite responses from the page: `Baking` resolves itself and must not alarm, `Unreadable` is
/// terminal until something changes and needs the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationFetchError {
    /// The endpoint answered `503` — the snapshot is still baking. Self-healing.
    Baking,
    /// Anything else: a transport failure, a non-`503` status, or a body that would not decode.
    Unreadable,
}

/// Classifies a non-`ok` population response by status code. Split out from
/// [`fetch_population_arrow`] so it is reachable from a host-target unit test — the fetch itself
/// needs a browser, which is how the 503 case went unnoticed in the first place.
pub fn population_error_for_status(status: u16) -> PopulationFetchError {
    if status == 503 {
        PopulationFetchError::Baking
    } else {
        PopulationFetchError::Unreadable
    }
}

/// The phase 44 UI spec "Loading — canvas": shown while the Arrow IPC population fetch and the
/// Cosmograph mount are in flight.
pub const CANVAS_LOADING_COPY: &str = "Loading the graph\u{2026}";

/// D-11's fixed warm-up window: 3 seconds after mount, the shim's `pause()` freezes the force
/// simulation. Never user-configurable — Re-layout (a later plan) is the only way to restart it.
pub(crate) const WARM_UP_DURATION: Duration = Duration::from_secs(3);

const PANEL_STYLE: &str = "background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-lg); display: flex; flex-direction: column; gap: var(--space-md); margin-top: var(--space-lg);";
const CAPTION_STYLE: &str = "color: var(--muted-foreground);";

/// Fetches and deserialises [`PopulationMeta`] from [`POPULATION_META_URL`]. Mirrors
/// `status_page.rs::fetch_status`'s exact shape.
async fn fetch_population_meta() -> Result<PopulationMeta, String> {
    let response = gloo_net::http::Request::get(POPULATION_META_URL)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.ok() {
        return Err(format!(
            "population meta responded with status {}",
            response.status()
        ));
    }
    response
        .json::<PopulationMeta>()
        .await
        .map_err(|err| err.to_string())
}

/// Fetches and deserialises [`StatusView`] from the SAME `/api/workbench-status` endpoint
/// [`STATUS_URL`] identifies — this page's node/edge counts come from here, since
/// `PopulationMeta` alone carries only the population `count`, not distinct node/edge counts
/// (per this plan's own `<interfaces>` note).
async fn fetch_workbench_status() -> Result<StatusView, String> {
    let response = gloo_net::http::Request::get(STATUS_URL)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.ok() {
        return Err(format!(
            "workbench-status responded with status {}",
            response.status()
        ));
    }
    response
        .json::<StatusView>()
        .await
        .map_err(|err| err.to_string())
}

/// Fetches [`POPULATION_ARROW_URL`]'s raw response body (the population's Arrow IPC export,
/// `gloo-net` 0.6's `Response::binary` — confirmed against this crate's pinned `gloo-net = "0.6"`
/// in `crates/workbench-web/Cargo.toml`). The bytes are handed to [`split_population_arrow`],
/// never parsed with the `arrow` crate on this side of the wasm boundary (see
/// [`parse_utf8_column`]'s doc comment).
async fn fetch_population_arrow() -> Result<Vec<u8>, PopulationFetchError> {
    let response = gloo_net::http::Request::get(POPULATION_ARROW_URL)
        .send()
        .await
        .map_err(|_| PopulationFetchError::Unreadable)?;
    if !response.ok() {
        return Err(population_error_for_status(response.status()));
    }
    response
        .binary()
        .await
        .map_err(|_| PopulationFetchError::Unreadable)
}

/// The population panel's headline: the count as a plain number when available, otherwise
/// "Population unavailable" — never a fabricated `0` (mirrors `status_page.rs::population_headline`
/// exactly, but over [`PopulationMeta`] rather than `PopulationView`).
pub fn population_panel_headline(meta: &PopulationMeta) -> String {
    match meta.count {
        Some(count) => count.to_string(),
        None => "Population unavailable".to_string(),
    }
}

/// The population-available panel: node/edge counts and the ward-seed count (from `StatusView`/
/// `PopulationMeta`, Display/`.font-mono`, all three in the same value+caption pattern — checkpoint
/// fix item 1 moved the ward-seed figure into this row so it is never a bare, unlabeled number),
/// the population count, and the population's generation `Timestamp`.
fn available_panel(status: &StatusView, meta: &PopulationMeta) -> AnyView {
    let node_count = status.node_count;
    let edge_count = status.edge_count;
    let population_headline = population_panel_headline(meta);
    let generated_at = meta.generated_at.clone();
    let ward_seed_count = meta.ward_seed_count;

    // ONE row, not three stacked. The panel sits directly above the canvas, so every pixel it
    // takes is a pixel the graph loses — at 1600x1000 the three-row form measured 171px tall and
    // pushed the canvas 370px down the page (operator QA 2026-09-12: "a little too tall and
    // reduces the amount of screen space available for the graph"). The population figure becomes
    // a fourth value+caption column rather than its own line, and the timestamp sits inline.
    // `available_panel` is only reached when `meta.count.is_some()`, so the headline here is
    // always a number and never the "Population unavailable" sentence.
    view! {
        <div style=format!("{PANEL_STYLE} padding: var(--space-md); gap: var(--space-sm); margin-top: var(--space-md);")>
            <div style="display: flex; gap: var(--space-lg); align-items: baseline; flex-wrap: wrap;">
                <div style="display: flex; flex-direction: column;">
                    <span class="text-display font-mono">{node_count.to_string()}</span>
                    <span class="text-label" style=CAPTION_STYLE>"nodes"</span>
                </div>
                <div style="display: flex; flex-direction: column;">
                    <span class="text-display font-mono">{edge_count.to_string()}</span>
                    <span class="text-label" style=CAPTION_STYLE>"edges"</span>
                </div>
                <div style="display: flex; flex-direction: column;">
                    <span class="text-display font-mono">{ward_seed_count.to_string()}</span>
                    <span class="text-label" style=CAPTION_STYLE>{WARD_SEED_CAPTION}</span>
                </div>
                <div style="display: flex; flex-direction: column;">
                    <span class="text-display font-mono">{population_headline}</span>
                    <span class="text-label" style=CAPTION_STYLE>"population"</span>
                </div>
                <Timestamp at=generated_at />
            </div>
        </div>
    }
    .into_any()
}

/// The D-11 population-unavailable panel: a fixed "Population unavailable" heading and the
/// server's typed reason verbatim, in place of the counts — no zero, no blank card.
fn unavailable_panel(meta: &PopulationMeta) -> AnyView {
    let reason = meta.unavailable_reason.clone().unwrap_or_default();

    view! {
        <div style=PANEL_STYLE>
            <span class="text-heading">"Population unavailable"</span>
            <span class="text-body" style=CAPTION_STYLE>{reason}</span>
        </div>
    }
    .into_any()
}

/// The honest state while a large replay's CSR bake has not yet landed (checkpoint fix item 4):
/// [`SNAPSHOT_BAKING_HEADING`] plus the population line — never a fabricated `0` node/edge count
/// in that window. Uses [`population_panel_headline`] directly, so it renders correctly whether
/// the population itself is available or not — orthogonal to whether the bake has landed.
fn baking_panel(meta: &PopulationMeta) -> AnyView {
    let population_headline = population_panel_headline(meta);
    let generated_at = meta.generated_at.clone();

    view! {
        <div style=PANEL_STYLE>
            <span class="text-heading">{SNAPSHOT_BAKING_HEADING}</span>
            <span class="text-body font-mono">"population " {population_headline}</span>
            <Timestamp at=generated_at />
        </div>
    }
    .into_any()
}

/// Pure branch-selection logic (host-testable, checkpoint fix item 4): true exactly when the
/// backend has flagged the CSR snapshot as still baking after a historical replay — the caller
/// renders [`baking_panel`] instead of node/edge counts, and keeps polling until this flips back
/// to `false`.
pub fn should_show_snapshot_baking(status: &StatusView) -> bool {
    status.snapshot_pending
}

/// Chooses [`baking_panel`], [`available_panel`] or [`unavailable_panel`] — [`baking_panel`]
/// takes priority over both (checkpoint fix item 4: while the CSR snapshot is still baking, the
/// node/edge counts are the empty initial snapshot's `0`s, not real data, regardless of whether
/// the population itself resolved); otherwise the existing `meta.count`-driven choice.
fn population_panel(status: &StatusView, meta: &PopulationMeta) -> AnyView {
    if should_show_snapshot_baking(status) {
        baking_panel(meta)
    } else if meta.count.is_some() {
        available_panel(status, meta)
    } else {
        unavailable_panel(meta)
    }
}

/// True exactly when the canvas/side-card section (this plan) should attempt to mount: the CSR
/// snapshot is not still baking AND a real population count is in hand. Mirrors
/// [`population_panel`]'s own dispatch so the two never disagree about whether "population is
/// available" holds (the phase 44 UI spec: population-unavailable/load-failure states supersede the
/// whole canvas/rail/side-card section).
fn should_mount_canvas(status: &StatusView, meta: &PopulationMeta) -> bool {
    !should_show_snapshot_baking(status) && meta.count.is_some()
}

// ─── Arrow IPC byte framing (no `arrow`/`arrow-ipc` crate dependency) ──────────────────────────
//
// `scripts/check-wasm-boundary.sh` forbids this crate from depending on the `arrow` crate by
// name (the phase 44 research notes: "The arrow crate never needs to compile to wasm32-unknown-unknown at
// all... this resolves what would otherwise be a real risk"), and Cosmograph's own
// `prepareCosmographData` parses Arrow IPC bytes entirely inside the vendored JS bundle — so this
// crate never needs a GENERAL Arrow reader. The one thing it DOES need, which nothing else in
// this phase's D-17 trait surface provides (`CosmographHandle` exposes no "point index -> id"
// accessor), is resolving a clicked canvas point's index back to its entity id for the side
// card. The functions below are a deliberately NARROW, hand-rolled reader — not a general
// FlatBuffers/Arrow library — sufficient only to: (1) split `POPULATION_ARROW_URL`'s two-stream
// wire format (`crates/workbench/src/graph/arrow_export.rs`'s documented framing) into its nodes
// and links sub-streams, and (2) extract one `Utf8`, non-nullable column's values, in row order,
// from a single-schema, single-record-batch Arrow IPC stream written by
// `arrow::ipc::writer::StreamWriter` (the writer `arrow_export.rs` actually uses). Every function
// here returns `None`/an empty `Vec` — never panics — on any malformed or unexpected input.

fn read_u32_le(bytes: &[u8], pos: usize) -> Option<u32> {
    bytes
        .get(pos..pos + 4)
        .map(|slice| u32::from_le_bytes(slice.try_into().expect("checked 4-byte slice")))
}

fn read_i32_le(bytes: &[u8], pos: usize) -> Option<i32> {
    read_u32_le(bytes, pos).map(|value| value as i32)
}

fn read_u16_le(bytes: &[u8], pos: usize) -> Option<u16> {
    bytes
        .get(pos..pos + 2)
        .map(|slice| u16::from_le_bytes(slice.try_into().expect("checked 2-byte slice")))
}

fn read_i64_le(bytes: &[u8], pos: usize) -> Option<i64> {
    bytes
        .get(pos..pos + 8)
        .map(|slice| i64::from_le_bytes(slice.try_into().expect("checked 8-byte slice")))
}

fn round_up_8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// Resolves FIELD `field_index`'s byte position WITHIN `buf`, given a FlatBuffers table starting
/// at `table_pos`. Returns `None` if the field is absent (never written by the encoder — the
/// FlatBuffers vtable convention for a default/omitted value) or if the bytes are malformed. This
/// is the one generic FlatBuffers primitive this reader needs — no other traversal shape (unions
/// beyond a single known member, nested vectors-of-vectors, dictionaries) is required for the
/// fixed `Message`/`RecordBatch`/`Buffer` schema shape this reader is written against (field IDs
/// from Apache Arrow's public, stable `Message.fbs` IDL).
fn fb_field_pos(buf: &[u8], table_pos: u32, field_index: u16) -> Option<u32> {
    let soffset = read_i32_le(buf, table_pos as usize)?;
    let vtable_pos: u32 = (table_pos as i64 - soffset as i64).try_into().ok()?;
    let vtable_size = read_u16_le(buf, vtable_pos as usize)?;
    let slot = 4usize + (field_index as usize) * 2;
    if (slot as u16) >= vtable_size {
        return None; // field absent — this table was built with fewer fields.
    }
    let field_rel = read_u16_le(buf, vtable_pos as usize + slot)?;
    if field_rel == 0 {
        return None; // field not written (default value).
    }
    Some(table_pos + field_rel as u32)
}

/// Resolves a `uoffset_t`-typed field at `field_pos` (as produced by [`fb_field_pos`]) into the
/// absolute position of the thing it points to (a nested table, string, or vector).
fn fb_indirect(buf: &[u8], field_pos: u32) -> Option<u32> {
    let rel = read_u32_le(buf, field_pos as usize)?;
    Some(field_pos + rel)
}

/// The root table position of a FlatBuffers-encoded buffer: the first 4 bytes are always a
/// `uoffset_t` to the root table.
fn fb_root(buf: &[u8]) -> Option<u32> {
    fb_indirect(buf, 0)
}

/// Parses ONE Arrow IPC "encapsulated message" header starting at `stream_bytes[pos..]`, per the
/// Arrow IPC streaming format: a 4-byte `0xFFFFFFFF` continuation marker, a 4-byte little-endian
/// metadata length, the FlatBuffers-encoded `Message` metadata (padded to an 8-byte boundary),
/// then the message body (also padded). Returns `(metadata_start, metadata_len, body_start,
/// body_len, next_message_pos)`. This reader does not need to inspect the metadata's CONTENT
/// beyond `Message.bodyLength` (field index 3) — enough to locate the body and the next message
/// — the caller decides what, if anything, to do with each message's own header/content.
fn read_message_frame(
    stream_bytes: &[u8],
    pos: usize,
) -> Option<(usize, usize, usize, usize, usize)> {
    let continuation = read_u32_le(stream_bytes, pos)?;
    let (meta_start, meta_len) = if continuation == 0xFFFF_FFFF {
        let len = read_i32_le(stream_bytes, pos + 4)?;
        if len <= 0 {
            return None; // EOS marker, or malformed — no message here either way.
        }
        (pos + 8, len as usize)
    } else {
        // Legacy (pre-continuation-marker) IPC framing: the first 4 bytes ARE the metadata
        // length directly. `arrow::ipc::writer::StreamWriter` (this project's own producer,
        // `arrow_export.rs`) always emits the continuation-marker form — this branch is a
        // defensive fallback only, never exercised against this project's own writer output.
        let len = continuation as i32;
        if len <= 0 {
            return None;
        }
        (pos + 4, len as usize)
    };
    let metadata = stream_bytes.get(meta_start..meta_start + meta_len)?;
    let message_table_pos = fb_root(metadata)?;
    // `Message.bodyLength` (field index 3) is ABSENT from the vtable whenever it equals its
    // FlatBuffers default of 0 — which is exactly the case for the SCHEMA message every Arrow IPC
    // stream opens with (a schema carries no body). An absent field means "the default", never
    // "malformed", so it must read as 0 here. Treating it as a parse failure aborted the whole
    // walk on message 1, so `parse_utf8_column` returned an empty Vec for every real stream
    // `arrow::ipc::writer::StreamWriter` produces: `id_to_index` and `node_ids` were empty at
    // runtime, and with them every canvas effect (dim/hide/recolour/resize/fit) silently no-opped
    // and no side card could open. The hand-built fixtures in this module's tests always WROTE
    // bodyLength (see `encode_bodylength_only_message`), so nothing caught it — hence the
    // real-arrow-bytes fixture in `tests`.
    let body_len = match fb_field_pos(metadata, message_table_pos, 3) {
        Some(field_pos) => read_i64_le(metadata, field_pos as usize)?,
        None => 0,
    };
    if body_len < 0 {
        return None;
    }
    let body_start = meta_start + meta_len;
    let body_len = body_len as usize;
    let next = body_start + round_up_8(body_len);
    Some((meta_start, meta_len, body_start, body_len, next))
}

/// Extracts column `column_index`'s `Utf8` values, in row order, from `stream_bytes` — a
/// single-schema, single-record-batch Arrow IPC STREAM as produced by
/// `arrow::ipc::writer::StreamWriter` (uncompressed, no dictionaries), matching
/// `arrow_export.rs`'s nodes-stream schema (`id`, `kind`, `name`, all `Utf8`, non-nullable) this
/// function is written against. Returns an empty `Vec` — never panics — on any malformed or
/// unexpected input; a side card that cannot resolve a clicked node's id simply does not open,
/// which is the correct honest-degradation behaviour for this project (never a fabricated id).
///
/// Deliberately narrow, NOT a general Arrow/FlatBuffers reader: it skips message 1 (the schema
/// message) entirely — this reader already knows the column layout statically from
/// `arrow_export.rs`'s documented schema, so parsing the schema message's own content is
/// unnecessary — and assumes every column is `Utf8` (never `LargeUtf8`, dictionary-encoded, or
/// carrying actual nulls; every column `arrow_export.rs` emits is `Utf8`, non-nullable). It reads
/// only the two buffers a `Utf8` array layout defines beyond its (unused here, since
/// non-nullable) validity buffer: an `i32` offsets array and a raw UTF-8 data buffer, per the
/// Arrow columnar format's "Variable-size Binary Layout" — each column always contributes exactly
/// 3 buffer slots (validity, offsets, data) to the record batch's flat `buffers` list, in schema
/// column order, REGARDLESS of nullability (the Arrow IPC spec's own documented optimisation: a
/// column with no nulls still gets a validity buffer *slot*, just with `length = 0`).
pub fn parse_utf8_column(stream_bytes: &[u8], column_index: usize) -> Vec<String> {
    parse_utf8_column_checked(stream_bytes, column_index).unwrap_or_default()
}

fn parse_utf8_column_checked(stream_bytes: &[u8], column_index: usize) -> Option<Vec<String>> {
    // Message 1 (schema) — skipped; only its framing (to find message 2's start) matters here.
    let (_, _, _, _, next) = read_message_frame(stream_bytes, 0)?;
    // Message 2 (record batch) — the one this function actually reads.
    let (meta_start, meta_len, body_start, body_len, _) = read_message_frame(stream_bytes, next)?;
    let metadata = stream_bytes.get(meta_start..meta_start + meta_len)?;
    let body = stream_bytes.get(body_start..body_start + body_len)?;

    let message_table_pos = fb_root(metadata)?;
    let header_field_pos = fb_field_pos(metadata, message_table_pos, 2)?; // Message.header
    let record_batch_pos = fb_indirect(metadata, header_field_pos)?;

    let buffers_field_pos = fb_field_pos(metadata, record_batch_pos, 2)?; // RecordBatch.buffers
    let buffers_vec_pos = fb_indirect(metadata, buffers_field_pos)?;
    let buffer_count = read_u32_le(metadata, buffers_vec_pos as usize)? as usize;

    const BUFFERS_PER_UTF8_COLUMN: usize = 3; // validity, offsets, data — see doc comment above.
    let offsets_buffer_index = column_index * BUFFERS_PER_UTF8_COLUMN + 1;
    let data_buffer_index = column_index * BUFFERS_PER_UTF8_COLUMN + 2;
    if data_buffer_index >= buffer_count {
        return None;
    }

    // `buffers` is a vector of `Buffer` STRUCTS (fixed 16 bytes: offset:i64, length:i64), stored
    // inline (no per-element uoffset indirection — struct vectors are contiguous in FlatBuffers).
    let read_buffer_struct = |i: usize| -> Option<(i64, i64)> {
        let elem_pos = buffers_vec_pos as usize + 4 + i * 16;
        let offset = read_i64_le(metadata, elem_pos)?;
        let length = read_i64_le(metadata, elem_pos + 8)?;
        Some((offset, length))
    };
    let (offsets_off, offsets_len) = read_buffer_struct(offsets_buffer_index)?;
    let (data_off, data_len) = read_buffer_struct(data_buffer_index)?;
    if offsets_off < 0 || offsets_len < 0 || data_off < 0 || data_len < 0 {
        return None;
    }

    let offsets_bytes =
        body.get(offsets_off as usize..(offsets_off as usize + offsets_len as usize))?;
    let data_bytes = body.get(data_off as usize..(data_off as usize + data_len as usize))?;

    if offsets_bytes.len() < 4 || !offsets_bytes.len().is_multiple_of(4) {
        return None;
    }
    let row_count = offsets_bytes.len() / 4 - 1;
    let mut out = Vec::with_capacity(row_count);
    for i in 0..row_count {
        let start = read_u32_le(offsets_bytes, i * 4)? as usize;
        let end = read_u32_le(offsets_bytes, (i + 1) * 4)? as usize;
        let slice = data_bytes.get(start..end)?;
        out.push(String::from_utf8(slice.to_vec()).ok()?);
    }
    Some(out)
}

/// Splits the raw `/api/graph/population` response body into `(nodes_stream_bytes,
/// links_stream_bytes)` — TWO independent, length-prefixed Arrow IPC streams concatenated, per
/// `crates/workbench/src/graph/arrow_export.rs::build_arrow_ipc`'s documented wire format:
/// `[nodes_len: u32 LE][nodes_bytes][links_len: u32 LE][links_bytes]`. Pure byte slicing — no
/// Arrow parsing needed for this outer framing. Returns `None` on a truncated/malformed buffer,
/// never panics.
///
/// Both halves are mounted: [`CosmographHandle::mount`] (plan 44-16) takes `nodes_bytes` AND
/// `links_bytes` as two SEPARATE arguments, matching Cosmograph's own `prepareCosmographData(config,
/// pointsData, linksData?)` — edges draw on the canvas from the moment it mounts.
pub fn split_population_arrow(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let nodes_len = read_u32_le(bytes, 0)? as usize;
    let nodes_start: usize = 4;
    let nodes_end = nodes_start.checked_add(nodes_len)?;
    let nodes_bytes = bytes.get(nodes_start..nodes_end)?;

    let links_len_pos = nodes_end;
    let links_len = read_u32_le(bytes, links_len_pos)? as usize;
    let links_start = links_len_pos.checked_add(4)?;
    let links_end = links_start.checked_add(links_len)?;
    let links_bytes = bytes.get(links_start..links_end)?;

    Some((nodes_bytes, links_bytes))
}

/// Mints a per-tab session id once (44-12's `<interfaces>` note: "a per-tab token minted by the
/// frontend is acceptable"): `js_sys::Date::now()` combined with `js_sys::Math::random()`, never
/// regenerated on a re-render since [`GraphPage`]'s component body runs exactly once per mount
/// (Leptos re-renders reactively, from signals inside the view, not by re-invoking the whole
/// component function) — the [`StoredValue`] this feeds is created once, right here.
///
/// `pub(crate)` since plan 50-07: `finding_detail_page.rs`'s own `ToolRail` mount needs a session
/// id of its own, minted the same way, rather than a second copy of this exact logic.
pub(crate) fn mint_session_id() -> String {
    let now = js_sys::Date::now() as u64;
    let random = (js_sys::Math::random() * 1e12) as u64;
    format!("session-{now}-{random}")
}

// ─── Result <-> canvas wiring (plan 44-13 Task 3) ──────────────────────────────────────────────
//
// The population's node ids (`node_ids`, in CSR-index order — index i's canvas point is
// `node_ids[i]`, per `parse_utf8_column`'s own row-order guarantee) is the ONLY thing that lets
// this wiring turn an `AlgorithmResult`'s entity ids back into the `CosmographHandle` indices its
// dim/hide/resize/fit methods need. `id_to_index` (built once, alongside `node_ids`, in the mount
// `Effect`) makes that resolution O(1) per id rather than an O(population) scan per id.

/// Resolves `ids` to their canvas point indices via `id_to_index`, silently skipping any id not
/// present (an id genuinely outside the mounted population — never expected in practice since
/// every algorithm runs over the same canonical population the canvas mounts, GRA-06 — but this
/// stays defensive rather than panicking).
fn resolve_indices(ids: &[String], id_to_index: &HashMap<String, u32>) -> Vec<u32> {
    ids.iter()
        .filter_map(|id| id_to_index.get(id).copied())
        .collect()
}

/// The node ids an outcome's AUTOMATIC (run-completion) effect hides, per the phase 44 UI spec's
/// automatic-effects table — shared by both the automatic-effect application itself and the
/// row-selection effect (which must layer its own dimming ON TOP of whatever the automatic effect
/// already hid, never un-hiding it). `Community`/`KCore` carry their hidden set directly
/// (`hidden_node_ids`, always the complete excluded set, never capped); `WardSubgraph` has no
/// `hidden_node_ids` field of its own — its outcome only carries the INCLUDED reachable set
/// (`node_ids`), so the hidden set is computed as `population_ids` minus that set.
/// `Betweenness`/`Paths`/`ShortestPaths` never hide anything automatically.
fn automatic_hidden_ids(outcome: &AlgorithmOutcome, population_ids: &[String]) -> Vec<String> {
    match outcome {
        AlgorithmOutcome::Community {
            hidden_node_ids, ..
        } => hidden_node_ids.clone(),
        AlgorithmOutcome::KCore {
            hidden_node_ids, ..
        } => hidden_node_ids.clone(),
        AlgorithmOutcome::WardSubgraph { node_ids, .. } => {
            let included: HashSet<&str> = node_ids.iter().map(|s| s.as_str()).collect();
            population_ids
                .iter()
                .filter(|id| !included.contains(id.as_str()))
                .cloned()
                .collect()
        }
        // A deep link hides everything it does not name, exactly as `WardSubgraph` hides
        // everything outside the reachable set.
        AlgorithmOutcome::HighlightSet { node_ids, .. } => {
            let included: HashSet<&str> = node_ids.iter().map(|s| s.as_str()).collect();
            population_ids
                .iter()
                .filter(|id| !included.contains(id.as_str()))
                .cloned()
                .collect()
        }
        AlgorithmOutcome::Betweenness { .. }
        | AlgorithmOutcome::Paths { .. }
        | AlgorithmOutcome::ShortestPaths { .. } => vec![],
    }
}

/// The node ids a results-list row at `row_index` represents, for the row-selection dim/fit effect
/// (D-12/D-14) — row ordering matches `graph_results_list.rs::row_count_for_outcome`'s own
/// counting exactly (both modules were written against the same per-algorithm row shape in
/// the phase 44 UI spec's "Results list" section):
///
/// - `Paths`/`ShortestPaths`: row `i`'s path's own hop entity ids.
/// - `Community`: row `i`'s community's [`CommunityResult::member_node_ids`] ids — the COMPLETE
///   membership of that community (44-17 closed the DTO gap 44-13 had documented: the wire
///   contract now carries every member, not just the capped `top_entities` the results list row
///   itself renders), so a community's row-selection highlight/fit covers the whole community.
/// - `KCore`: row `0` (the summary row) is the WHOLE surviving core (`population_ids` minus
///   `hidden_node_ids` — complete, not an approximation, since `hidden_node_ids` is never capped);
///   row `i > 0` is `top_nodes[i - 1]`'s single entity id.
/// - `Betweenness`: row `i`'s single ranked entity id.
/// - `WardSubgraph`: the one summary row is the outcome's own `node_ids` — the COMPLETE induced
///   reachable set (not capped), so no approximation here.
fn selected_row_node_ids(
    outcome: &AlgorithmOutcome,
    row_index: usize,
    population_ids: &[String],
) -> Vec<String> {
    match outcome {
        AlgorithmOutcome::Paths { paths } | AlgorithmOutcome::ShortestPaths { paths, .. } => paths
            .get(row_index)
            .map(|path| path.hops.iter().map(|hop| hop.entity.id.clone()).collect())
            .unwrap_or_default(),
        AlgorithmOutcome::Community { communities, .. } => communities
            .get(row_index)
            .map(|community| community.member_node_ids.clone())
            .unwrap_or_default(),
        AlgorithmOutcome::KCore {
            top_nodes,
            hidden_node_ids,
            ..
        } => {
            if row_index == 0 {
                let hidden: HashSet<&str> = hidden_node_ids.iter().map(|s| s.as_str()).collect();
                population_ids
                    .iter()
                    .filter(|id| !hidden.contains(id.as_str()))
                    .cloned()
                    .collect()
            } else {
                top_nodes
                    .get(row_index - 1)
                    .map(|ranked| vec![ranked.entity.id.clone()])
                    .unwrap_or_default()
            }
        }
        AlgorithmOutcome::Betweenness { top_nodes, .. } => top_nodes
            .get(row_index)
            .map(|ranked| vec![ranked.entity.id.clone()])
            .unwrap_or_default(),
        AlgorithmOutcome::WardSubgraph { node_ids, .. } => node_ids.clone(),
        // The deep link's whole set is one row, so selecting it selects every named entity —
        // the same shape as `WardSubgraph`'s single summary row.
        AlgorithmOutcome::HighlightSet { node_ids, .. } => node_ids.clone(),
    }
}

/// Applies `outcome`'s automatic (run-completion) canvas effect through `handle`, per
/// [`automatic_effect_for`]'s classification — `KCore` and `WardSubgraph` both classify as
/// [`CanvasEffect::Hide`] but compute their hidden id sets differently (see
/// [`automatic_hidden_ids`]'s own doc comment), so this still matches on `outcome` itself inside
/// the `Hide` arm rather than trying to carry that distinction inside `CanvasEffect`.
/// The ids still showing once `hidden_ids` are hidden — the input to the canvas label filter.
/// An EMPTY return means "no filter", which restores every label.
///
/// A hide is expressed by zeroing a point's colour alpha, and Cosmograph's label renderer never
/// reads that buffer, so the label set has to be narrowed separately or the labels of
/// filtered-out nodes keep floating over the empty space where those nodes were.
fn visible_ids_after_hiding(population_ids: &[String], hidden_ids: &[String]) -> Vec<String> {
    if hidden_ids.is_empty() {
        return Vec::new();
    }
    let hidden: HashSet<&str> = hidden_ids.iter().map(|s| s.as_str()).collect();
    population_ids
        .iter()
        .filter(|id| !hidden.contains(id.as_str()))
        .cloned()
        .collect()
}

/// Put the canvas into the state ONE algorithm result implies — the raw, kind-coloured,
/// full-opacity population when there is no result, and [`apply_automatic_effect`]'s own
/// run-completion effect when there is.
///
/// Extracted (plan 45-13) because two effects now need it: the `algorithm_result` effect, and the
/// neighbourhood-expansion effect's restore path. A second hand-written copy of "restore the raw
/// population" is how the two would come to disagree about what "no result" looks like.
pub(crate) fn apply_result_state(
    handle: &CosmographHandle,
    result: Option<&AlgorithmResult>,
    id_to_index: &HashMap<String, u32>,
    population_ids: &[String],
) {
    match result {
        None => {
            handle.set_point_opacity(&[], &[]);
            handle.clear_point_color_overrides();
            handle.set_visible_labels(&[]);
        }
        Some(result) => {
            apply_automatic_effect(handle, &result.outcome, id_to_index, population_ids);
        }
    }
}

fn apply_automatic_effect(
    handle: &CosmographHandle,
    outcome: &AlgorithmOutcome,
    id_to_index: &HashMap<String, u32>,
    population_ids: &[String],
) {
    // D-12/T-44-13-01: always clear any hue overrides from a PRIOR run before applying this run's
    // — never lets a stale community hue linger on a node the new run's calls never touch.
    // `clear_point_color_overrides` does not touch the opacity dim/hide state (its own doc
    // comment), so this is safe to call unconditionally here.
    handle.clear_point_color_overrides();

    match automatic_effect_for(outcome) {
        // No hide, so nothing narrows the label set — clear any filter a previous run left.
        CanvasEffect::None => handle.set_visible_labels(&[]),
        CanvasEffect::Recolor => {
            if let AlgorithmOutcome::Community {
                communities,
                hidden_node_ids,
            } = outcome
            {
                for community in communities {
                    let indices = resolve_indices(&community.member_node_ids, id_to_index);
                    handle.set_point_color_by_hue(&indices, community.color_hue_deg as f64);
                }
                let hidden_indices = resolve_indices(hidden_node_ids, id_to_index);
                handle.set_point_opacity(&[], &hidden_indices);
                let visible = visible_ids_after_hiding(population_ids, hidden_node_ids);
                handle.set_visible_labels(&visible);
            }
        }
        CanvasEffect::Hide => {
            let hidden_ids = automatic_hidden_ids(outcome, population_ids);
            let hidden_indices = resolve_indices(&hidden_ids, id_to_index);
            handle.set_point_opacity(&[], &hidden_indices);
            let visible = visible_ids_after_hiding(population_ids, &hidden_ids);
            handle.set_visible_labels(&visible);
        }
        CanvasEffect::Resize => {
            if let AlgorithmOutcome::Betweenness { top_nodes, .. } = outcome {
                let max_score = top_nodes.iter().map(|r| r.score).fold(0.0_f64, f64::max);
                let mut indices = Vec::with_capacity(top_nodes.len());
                let mut scores = Vec::with_capacity(top_nodes.len());
                for ranked in top_nodes {
                    if let Some(&idx) = id_to_index.get(&ranked.entity.id) {
                        indices.push(idx);
                        scores.push(betweenness_resize_scale(ranked.score, max_score));
                    }
                }
                handle.set_point_size_by_score(&indices, &scores);
                handle.set_visible_labels(&[]);
            }
        }
    }
}

/// Resolves one deep-link id to a full [`EntityRef`] through the same search endpoint the entity
/// picker uses. Returns `None` when the id names nothing the server knows, so the caller can show
/// the out-of-scope copy rather than running an algorithm against a phantom endpoint.
async fn resolve_ref(id: &str) -> Option<EntityRef> {
    let matches = entity_picker::fetch_entity_search(id.to_string())
        .await
        .ok()?;
    matches.into_iter().find(|entity| entity.id == id)
}

/// Acts on a parsed deep link once the canvas is mounted (plan 44-21, spec §6.2).
fn apply_deep_link(
    action: graph_deep_link::DeepLinkAction,
    state: ToolRailSignals,
    id_to_index: StoredValue<HashMap<String, u32>, LocalStorage>,
    cosmograph: StoredValue<Option<CosmographHandle>, LocalStorage>,
    clicked_index: RwSignal<Option<u32>>,
    point_hit_this_click: RwSignal<bool>,
) {
    use graph_deep_link::DeepLinkAction as Action;
    match action {
        Action::None => {}
        Action::SelectOne(id) => {
            match id_to_index.with_value(|map| map.get(&id).copied()) {
                Some(idx) => {
                    // Same two signals a real point click sets, so the side card opens exactly as
                    // it would had the operator clicked the node themselves.
                    point_hit_this_click.set(true);
                    clicked_index.set(Some(idx));
                    cosmograph.with_value(|handle| {
                        if let Some(handle) = handle {
                            handle.select_points(&[idx]);
                            handle.fit_view_by_indices(&[idx]);
                        }
                    });
                }
                None => state
                    .algorithm_error
                    .set(Some(tool_rail::out_of_scope_copy(&id))),
            }
        }
        Action::ShortestPath { from, to } => {
            run_with_endpoints(state, "shortest_paths".to_string(), from, to)
        }
        Action::RunWithEndpoints {
            algorithm,
            from,
            to,
        } => run_with_endpoints(state, algorithm, from, to),
        Action::HighlightSet(ids) => tool_rail::start_highlight(state, ids),
    }
}

/// Resolves both endpoints, then starts the run through the rail's own entry point — so a
/// deep-linked run is indistinguishable from one the operator started, including its in-flight
/// guard and error copy. Either endpoint failing to resolve stops the run rather than sending a
/// half-specified request.
fn run_with_endpoints(state: ToolRailSignals, algorithm: String, from: String, to: String) {
    leptos::task::spawn_local(async move {
        let resolved_from = resolve_ref(&from).await;
        let resolved_to = resolve_ref(&to).await;
        match (resolved_from, resolved_to) {
            (Some(resolved_from), Some(resolved_to)) => {
                state.algorithm.set(algorithm);
                state.from_ref.set(Some(resolved_from));
                state.to_ref.set(Some(resolved_to));
                tool_rail::start_run(state);
            }
            (None, _) => state
                .algorithm_error
                .set(Some(tool_rail::out_of_scope_copy(&from))),
            (_, None) => state
                .algorithm_error
                .set(Some(tool_rail::out_of_scope_copy(&to))),
        }
    });
}

/// The `/graph` page: `Breadcrumb`, an `<h1>"Graph"</h1>`, the population panel (unchanged from
/// Phase 43), then — once the population is genuinely available ([`should_mount_canvas`]) — the
/// canvas section: a "Loading the graph…" state while [`POPULATION_ARROW_URL`] and the Cosmograph
/// mount are in flight, [`POPULATION_ARROW_FETCH_ERROR_COPY`] + Retry on a fetch/framing failure
/// (canvas not mounted), otherwise the mounted canvas plus the node-click [`SideCard`].
///
/// The canvas mounts EXACTLY ONCE via [`CosmographHandle::mount`] (D-17), outside Leptos's
/// reactive path, guarded on an `already_mounted` check mirroring `cosmograph_spike.rs`'s proven
/// pattern; it warms up for [`WARM_UP_DURATION`] then freezes (D-11). `Escape` and clicking empty
/// canvas both close the side card (D-12) via two `window`-level listeners registered once and
/// removed on page leave.
#[component]
#[allow(non_snake_case)]
pub fn GraphPage() -> impl IntoView {
    let status = LocalResource::new(fetch_workbench_status);
    let meta = LocalResource::new(fetch_population_meta);
    let population_arrow = LocalResource::new(fetch_population_arrow);

    let canvas_ref: NodeRef<leptos::html::Div> = NodeRef::new();
    let cosmograph: StoredValue<Option<CosmographHandle>, LocalStorage> =
        StoredValue::new_local(None);
    let node_ids: StoredValue<Vec<String>, LocalStorage> = StoredValue::new_local(Vec::new());
    // Plan 44-13 Task 3: the id -> canvas-index reverse lookup, built once alongside `node_ids`
    // (in the mount `Effect` below) so every later result/row-selection wiring pass resolves ids
    // to indices in O(1) rather than re-scanning the whole population per call.
    let id_to_index: StoredValue<HashMap<String, u32>, LocalStorage> =
        StoredValue::new_local(HashMap::new());
    let clicked_index: RwSignal<Option<u32>> = RwSignal::new(None);
    let point_hit_this_click: RwSignal<bool> = RwSignal::new(false);
    let canvas_error: RwSignal<Option<String>> = RwSignal::new(None);
    // Plan 44-13: the results-list row currently SELECTED (committed via click/Enter/Space,
    // distinct from the list's own internal keyboard FOCUS) — drives the canvas dim/fit wiring
    // below (D-12/D-14).
    let selected_row: RwSignal<Option<usize>> = RwSignal::new(None);

    // Plan 44-12: the tool rail's shared reactive state. `session_id` is minted exactly once
    // here; `edge_types_included` is seeded to the D-01 defaults so the checklist's initial
    // render already matches D-01 before any user interaction.
    let session_id: StoredValue<String, LocalStorage> = StoredValue::new_local(mint_session_id());
    let edge_types_included: RwSignal<Vec<String>> =
        RwSignal::new(tool_rail::default_edge_types_included());
    let algorithm: RwSignal<String> = RwSignal::new("community".to_string());
    let from_ref: RwSignal<Option<EntityRef>> = RwSignal::new(None);
    let to_ref: RwSignal<Option<EntityRef>> = RwSignal::new(None);
    let ward_ref: RwSignal<Option<EntityRef>> = RwSignal::new(None);
    let seed: RwSignal<u64> = RwSignal::new(urban_integrity_workbench_dto::caps::DEFAULT_SEED);
    let hop_cap: RwSignal<u32> = RwSignal::new(4);
    let result_cap: RwSignal<u32> = RwSignal::new(100);
    let budget_secs: RwSignal<u32> = RwSignal::new(5);
    let k: RwSignal<u32> = RwSignal::new(3);
    let top_n: RwSignal<u32> = RwSignal::new(50);
    let min_community_size: RwSignal<u32> = RwSignal::new(5);
    let resolution: RwSignal<f64> = RwSignal::new(1.0);
    let algorithm_result: RwSignal<Option<AlgorithmResult>> = RwSignal::new(None);
    let algorithm_running: RwSignal<bool> = RwSignal::new(false);
    let algorithm_error: RwSignal<Option<String>> = RwSignal::new(None);

    // Plan 45-13 (CNV-01): the neighbourhood expansion. `expansion` is the whole of it — the
    // server's answer, carrying the applied hop budget, the applied node cap and the mandatory
    // truncation block — so nothing on screen has to remember what was requested.
    let expansion: RwSignal<Option<NeighbourhoodView>> = RwSignal::new(None);
    let expansion_hops: RwSignal<u32> = RwSignal::new(neighbourhood::EXPANSION_DEFAULT_HOPS);
    let expansion_running: RwSignal<bool> = RwSignal::new(false);
    let expansion_error: RwSignal<Option<String>> = RwSignal::new(None);
    // CNV-02's cluster glyphs: the parent ids whose clusters are folded away. The canvas effect and
    // the list both read this one signal, so the points and the rows describing them cannot
    // disagree about what is hidden.
    let collapsed_clusters: RwSignal<Vec<String>> = RwSignal::new(Vec::new());

    // Plan 45-14 (CNV-03): the five structured filters. `population_ready` exists because the
    // filter's first request needs the mounted population's ids, and `node_ids` is a
    // `StoredValue` — untracked by construction, so nothing downstream of the mount would ever be
    // scheduled by it. One boolean signal, flipped once, is what makes the filter effect run after
    // the mount rather than before it.
    let population_ready: RwSignal<bool> = RwSignal::new(false);
    let canvas_filter_state = CanvasFilterSignals {
        node_kinds: RwSignal::new(Vec::new()),
        edge_types: RwSignal::new(Vec::new()),
        from_date: RwSignal::new(None),
        to_date: RwSignal::new(None),
        undated_edges: RwSignal::new(MissingEvidence::Keep),
        ward_id: RwSignal::new(None),
        min_p_c: RwSignal::new(None),
        unscored_nodes: RwSignal::new(MissingEvidence::Exclude),
        facets: RwSignal::new(None),
        result: RwSignal::new(None),
        running: RwSignal::new(false),
        error: RwSignal::new(None),
        seeded: RwSignal::new(false),
    };

    // The deep link is read ONCE, untracked, and cleared after it is applied: a link describes an
    // intent to arrive at a view, not a view to be re-entered every time a signal changes.
    let query = use_query_map();
    let deep_link: StoredValue<Option<graph_deep_link::DeepLinkAction>, LocalStorage> =
        StoredValue::new_local({
            let q = query.get_untracked();
            let action = graph_deep_link::plan(
                q.get("from").as_deref(),
                q.get("to").as_deref(),
                q.get("ids").as_deref(),
                q.get("algorithm").as_deref(),
            );
            (action != graph_deep_link::DeepLinkAction::None).then_some(action)
        });

    let tool_rail_state = ToolRailSignals {
        algorithm,
        edge_types: edge_types_included,
        from_ref,
        to_ref,
        ward_ref,
        seed,
        hop_cap,
        result_cap,
        budget_secs,
        k,
        top_n,
        min_community_size,
        resolution,
        session_id,
        algorithm_result,
        algorithm_running,
        algorithm_error,
        population_node_ids: node_ids,
        // The Graph page always runs over the whole population — only the finding detail route
        // (`finding_detail_page.rs`) ever sets this (plan 50-11's scoped-run fix).
        scope_ids: RwSignal::new(Vec::new()),
    };

    Effect::new(move |_| {
        if let Some(Ok(status_view)) = status.get() {
            if should_show_snapshot_baking(&status_view) {
                set_timeout(move || status.refetch(), Duration::from_secs(5));
            }
        }
    });

    // The population resource fires at mount, which on a large corpus is minutes before the CSR
    // bake lands — so its FIRST answer is routinely the handler's honest `503`. Without this it
    // cached that error for the life of the page: `should_mount_canvas` flipped true when the bake
    // finished, the canvas block rendered, and it read a stale `Err` from before the data existed.
    // The operator saw a permanent "cannot read" error over a population that was fine (QA-gate
    // defect log, 2026-09-13). Re-requesting on a `Baking` error is what makes the page self-heal.
    Effect::new(move |_| {
        if let Some(Err(PopulationFetchError::Baking)) = population_arrow.get() {
            set_timeout(move || population_arrow.refetch(), POPULATION_BAKING_RETRY);
        }
    });

    // D-11/D-17/CNV-04: mount the canvas exactly once, once BOTH the mount `<div>` exists AND the
    // population Arrow fetch has resolved `Ok` — mirrors `cosmograph_spike.rs`'s mount-exactly-
    // once `Effect` shape (guard on `already_mounted`, only mount once the data is in hand).
    Effect::new(move |_| {
        let Some(el) = canvas_ref.get() else {
            return;
        };
        let already_mounted = cosmograph.with_value(|handle| handle.is_some());
        if already_mounted {
            return;
        }
        let Some(Ok(bytes)) = population_arrow.get() else {
            return;
        };
        let Some((nodes_bytes, links_bytes)) = split_population_arrow(&bytes) else {
            canvas_error.set(Some(POPULATION_ARROW_FETCH_ERROR_COPY.to_string()));
            return;
        };
        let ids = parse_utf8_column(nodes_bytes, 0);
        id_to_index.set_value(
            ids.iter()
                .enumerate()
                .map(|(i, id)| (id.clone(), i as u32))
                .collect(),
        );
        node_ids.set_value(ids);
        // The filter's first request needs these ids; see `population_ready`'s own comment.
        population_ready.set(true);

        let on_click = move |index: u32| {
            point_hit_this_click.set(true);
            clicked_index.set(Some(index));
        };
        let kind_colors = node_palette::kind_color_entries();
        let edge_colors = edge_palette::edge_color_entries();
        let handle = CosmographHandle::mount(
            crate::cosmograph_trait::MountParams {
                container: &el,
                nodes_bytes,
                links_bytes,
                kind_colors: &kind_colors,
                edge_colors: &edge_colors,
                warm_up: WARM_UP_DURATION,
            },
            on_click,
            move |message| {
                canvas_error.set(Some(message));
            },
        );
        cosmograph.set_value(Some(handle));

        // Applied only after the handle exists: SelectOne needs to drive the canvas, and the
        // algorithm variants need `id_to_index` populated to resolve their results.
        if let Some(action) = deep_link.get_value() {
            deep_link.set_value(None);
            apply_deep_link(
                action,
                tool_rail_state,
                id_to_index,
                cosmograph,
                clicked_index,
                point_hit_this_click,
            );
        }

        // D-11's warm-up-end freeze — a fixed 3-second window, never user-configurable — is armed
        // inside the shim, from Cosmograph's own `onSimulationStart`, because the simulation does
        // not begin at mount: duckdb-wasm has to initialise and the data has to upload first,
        // which on a cold load takes longer than the window itself. A `set_timeout` started here
        // fired while `isSimulationRunning` was still false, and Cosmograph's `pause()` is a
        // silent no-op in that state — so the layout never froze at all.
    });

    // D-12: `Escape` closes the side card. Clicking empty canvas also closes it, distinguished
    // from a point click via `point_hit_this_click` — set synchronously by the point-click
    // handler above, which (per standard DOM bubble-phase ordering) fires before this
    // `window`-level listener for the SAME click event, since Cosmograph attaches its own click
    // handling closer to the canvas element than `window`. Both listeners are added once here and
    // removed on page leave, mirroring `leptos_dom::helpers::window_event_listener`'s own
    // documented `on_cleanup` pattern.
    let keydown_handle = window_event_listener_untyped("keydown", move |ev: web_sys::Event| {
        if let Some(keyboard_event) = ev.dyn_ref::<web_sys::KeyboardEvent>() {
            if keyboard_event.key() == "Escape" {
                clicked_index.set(None);
            }
        }
    });
    // the phase 44 UI spec Canvas contract: "Clicking empty canvas clears the current row selection...
    // and closes the side card if one is open" — the SAME empty-canvas click handler does both
    // (D-12's lighter clearing action, distinct from Reset's heavier one).
    let click_handle = window_event_listener_untyped("click", move |_ev: web_sys::Event| {
        if point_hit_this_click.get_untracked() {
            point_hit_this_click.set(false);
        } else {
            clicked_index.set(None);
            selected_row.set(None);
        }
    });

    // Plan 44-13 Task 3: applies the AUTOMATIC (run-completion) canvas effect whenever
    // `algorithm_result` changes. `None` (no active result — including immediately after an
    // algorithm switch, per `tool_rail.rs`'s `on_algorithm_change`) restores the raw,
    // kind-coloured, full-opacity population — the SAME restore `on_reset` below relies on
    // reactively, rather than duplicating these two calls in both places. A fresh `Some(result)`
    // also resets `selected_row` to `None` — a stale row index from a PREVIOUS result would
    // otherwise point at the wrong row once a new result replaces it.
    Effect::new(move |_| match algorithm_result.get() {
        None => {
            cosmograph.with_value(|handle| {
                if let Some(handle) = handle {
                    apply_result_state(handle, None, &HashMap::new(), &[]);
                }
            });
        }
        Some(result) => {
            selected_row.set(None);
            // A run and an expansion drive the SAME canvas channel (opacity + labels), so both
            // being active at once would leave the status line describing one and the canvas
            // showing the other. Clearing the expansion here schedules the expansion effect
            // below, whose `None` branch re-applies this very result — so the canvas lands in
            // one state whichever effect writes last.
            expansion.set(None);
            let population = node_ids.with_value(|ids| ids.clone());
            let index_map = id_to_index.with_value(|map| map.clone());
            cosmograph.with_value(|handle| {
                if let Some(handle) = handle {
                    apply_automatic_effect(handle, &result.outcome, &index_map, &population);
                }
            });
        }
    });

    // Plan 44-13 Task 3: the row-selection dim/fit effect (D-12/D-14), layered ON TOP of whatever
    // automatic effect the `Effect` above already applied — `None` (no row selected, including
    // the empty-canvas-click/Reset paths) restores to "whatever the automatic effect left the
    // canvas in" per the phase 44 UI spec, never all the way back to raw population by itself.
    Effect::new(move |_| {
        // Read the SELECTION FIRST, before any early return. Leptos 0.8 records a dependency only
        // for signals actually read during a given run, and on this effect's very first run (at
        // mount, before any algorithm has been run) `algorithm_result` is `None` — so returning
        // on that guard before touching `selected_row` left the effect with zero dependencies,
        // and it was never scheduled again. The canvas then ignored every row click for the rest
        // of the session, for every algorithm, which is exactly what the QA gate saw on
        // 2026-09-10: a shortest-paths run listed six paths and selecting any of them did
        // nothing. Reading it unconditionally is what keeps this effect subscribed.
        let selection = selected_row.get();
        let Some(result) = algorithm_result.get_untracked() else {
            return;
        };
        let population = node_ids.with_value(|ids| ids.clone());
        let index_map = id_to_index.with_value(|map| map.clone());
        let hidden_ids = automatic_hidden_ids(&result.outcome, &population);
        let hidden_indices = resolve_indices(&hidden_ids, &index_map);

        match selection {
            None => {
                cosmograph.with_value(|handle| {
                    if let Some(handle) = handle {
                        handle.set_point_opacity(&[], &hidden_indices);
                        let visible = visible_ids_after_hiding(&population, &hidden_ids);
                        handle.set_visible_labels(&visible);
                    }
                });
            }
            Some(row_index) => {
                let selected_ids = selected_row_node_ids(&result.outcome, row_index, &population);
                let selected_indices = resolve_indices(&selected_ids, &index_map);
                let hidden_set: HashSet<u32> = hidden_indices.iter().copied().collect();
                let selected_set: HashSet<u32> = selected_indices.iter().copied().collect();
                let population_len = population.len() as u32;
                let dimmed: Vec<u32> = (0..population_len)
                    .filter(|i| !selected_set.contains(i) && !hidden_set.contains(i))
                    .collect();
                // Operator decision at the plan 44-14 QA gate (2026-09-08), amending D-12's
                // "dim everything else": selecting a row shows THAT row's subgraph — every
                // non-member point (and, via the shim, every edge touching one) is hidden, not
                // dimmed. Against 113k points a 0.12-alpha haze still buried the community.
                let mut hidden_with_rest = hidden_indices.clone();
                hidden_with_rest.extend(dimmed.iter().copied());
                cosmograph.with_value(|handle| {
                    if let Some(handle) = handle {
                        handle.set_point_opacity(&[], &hidden_with_rest);
                        // Everything except this row is hidden, so the row's own ids ARE the
                        // visible set the labels must narrow to.
                        handle.set_visible_labels(&selected_ids);
                        handle.fit_view_by_indices(&selected_indices);
                    }
                });
            }
        }
    });
    // Plan 45-14 (CNV-03): one request per change of the pickers OR of the canvas's focus set.
    //
    // A filter **composes** with an expansion rather than replacing it — the focus set travels to
    // the server as `scope_ids`, so "filter" means "of what is in view", which is the only reading
    // under which a saved subgraph can equal what was on screen.
    //
    // Every signal this writes (`facets`, `result`, `running`, `error`) is read untracked, or the
    // effect would schedule itself. The picker signals it *does* track are the whole point.
    Effect::new(move |_| {
        if !population_ready.get() {
            return;
        }
        // `request_from` reads every picker signal TRACKED, so a click on any of the five
        // re-runs this effect. Leptos 0.8 records a dependency only for signals actually read in a
        // given run (the 44-13 defect that cost a session's row clicks, repeated in 45-13's own
        // expansion effect), which is why the composition happens here rather than behind a guard.
        let view = expansion.get();
        let collapsed = collapsed_clusters.get();
        let population = node_ids.with_value(|ids| ids.clone());
        let scope_ids = canvas_filters::scope_ids_for(view.as_ref(), &collapsed, &population);
        let request = canvas_filters::request_from(canvas_filter_state, scope_ids);

        canvas_filter_state.running.set(true);
        canvas_filter_state.error.set(None);
        leptos::task::spawn_local(async move {
            match canvas_filters::fetch_canvas_filter(request).await {
                Ok(answer) => {
                    canvas_filters::seed_checklists(canvas_filter_state, &answer.facets);
                    canvas_filter_state.facets.set(Some(answer.facets.clone()));
                    canvas_filter_state.result.set(Some(answer));
                }
                Err(message) => canvas_filter_state.error.set(Some(message)),
            }
            canvas_filter_state.running.set(false);
        });
    });

    // Plan 45-13 (CNV-01) / plan 45-14 (CNV-03): the canvas focus effect, created LAST so that in
    // a batch where it and the `algorithm_result` effect above are both scheduled, this one writes
    // last and the canvas ends in the state the status line is describing.
    //
    // 45-13 left a warning for this plan: the opacity/label channel had three writers, and a
    // filter would be a fourth whose precedence was an ordering accident. So the filter is **not**
    // a fourth writer. It is folded into this one, which already computed the focus set, and the
    // composition is a function call — `canvas_filters::visible_subgraph_ids`, the same one the
    // save reads. Two writers were left alone (the algorithm-result effect and the row-selection
    // effect); collapsing those as well is still the right cleanup and is still not this plan's.
    //
    // An expansion narrows the canvas rather than adding to it: the mounted population is fixed at
    // mount (`CosmographHandle` has no method that grows it), so both "expand this neighbourhood"
    // and "filter to these" are expressed as "hide everything that is not in it".
    Effect::new(move |_| {
        let focus = expansion.get();
        // Read unconditionally, before any branch: a collapse and a filter change must each re-run
        // this effect, and Leptos 0.8 records a dependency only on signals actually read during a
        // given run (the 44-13 defect that cost a whole session's row clicks).
        let collapsed = collapsed_clusters.get();
        let filter = canvas_filter_state.result.get();
        let filter_narrows = filter.as_ref().is_some_and(|f| f.filter_active);
        let population = node_ids.with_value(|ids| ids.clone());
        let index_map = id_to_index.with_value(|map| map.clone());
        cosmograph.with_value(|handle| {
            let Some(handle) = handle else {
                return;
            };
            // Neither an expansion nor a filter is narrowing anything: restore whatever the
            // current algorithm result implies — never unconditionally the raw population, which
            // would silently undo a run.
            if focus.is_none() && !filter_narrows {
                handle.set_point_size_by_score(&[], &[]);
                apply_result_state(
                    handle,
                    algorithm_result.get_untracked().as_ref(),
                    &index_map,
                    &population,
                );
                return;
            }

            let visible = canvas_filters::visible_subgraph_ids(
                focus.as_ref(),
                &collapsed,
                filter.as_ref(),
                &population,
            );
            let visible_set: HashSet<&str> = visible.iter().map(String::as_str).collect();
            let hidden: Vec<u32> = population
                .iter()
                .enumerate()
                .filter(|(_, id)| !visible_set.contains(id.as_str()))
                .map(|(i, _)| i as u32)
                .collect();
            handle.set_point_opacity(&[], &hidden);
            handle.set_visible_labels(&visible);

            // A collapsed cluster's parent IS the glyph: there is no way to add a synthetic point
            // to a mounted Cosmograph, so it is an existing point made large enough to read as the
            // many it stands for. Empty slices clear every size override, which is what a
            // re-expansion — and a filtered canvas with no expansion behind it — needs.
            match focus.as_ref() {
                Some(view) => {
                    let sizes = neighbourhood::glyph_sizes(view, &collapsed);
                    let glyph_indices = resolve_indices(
                        &sizes.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
                        &index_map,
                    );
                    let glyph_scores: Vec<f64> = sizes.iter().map(|(_, score)| *score).collect();
                    handle.set_point_size_by_score(&glyph_indices, &glyph_scores);
                }
                None => handle.set_point_size_by_score(&[], &[]),
            }

            // Fitting to nothing is not a view. A filter combination that empties the canvas is
            // answered with a sentence (`canvas_filters::empty_canvas_sentence`), never with a
            // camera move to an empty rectangle.
            let indices = resolve_indices(&visible, &index_map);
            if !indices.is_empty() {
                handle.fit_view_by_indices(&indices);
            }
        });
    });

    on_cleanup(move || {
        keydown_handle.remove();
        click_handle.remove();
        // ROADMAP success criterion 4 / the phase 44 UI spec QA checklist item 4: leaving the Graph page
        // while a Run is in flight cancels it via the SAME cancel endpoint Cancel run uses — the
        // component tree is already being torn down, so this is fire-and-forget (never blocks
        // navigation on the response), but it MUST actually issue the request.
        if algorithm_running.get_untracked() {
            let session_id_value = session_id.get_value();
            leptos::task::spawn_local(async move {
                tool_rail::trigger_algorithm_cancel(session_id_value).await;
            });
        }
    });

    // Plan 45-13 (CNV-01): the hop control's two actions. The seed is the clicked canvas point —
    // its id, because that is what the mounted Arrow column resolves a click to; the name arrives
    // with the inspection panel's own fetch and is not worth a second round trip here.
    let seed_label = Signal::derive(move || {
        clicked_index
            .get()
            .and_then(|idx| node_ids.with_value(|ids| ids.get(idx as usize).cloned()))
    });
    let on_expand = move || {
        let Some(id) = seed_label.get_untracked() else {
            return;
        };
        let hops = expansion_hops.get_untracked();
        expansion_running.set(true);
        expansion_error.set(None);
        // A parent id from the PREVIOUS expansion means nothing in this one, and a stale entry
        // would fold away a cluster nobody asked about.
        collapsed_clusters.set(Vec::new());
        leptos::task::spawn_local(async move {
            match neighbourhood::fetch_neighbourhood(
                id,
                hops,
                neighbourhood::EXPANSION_DEFAULT_NODE_CAP,
            )
            .await
            {
                Ok(view) => expansion.set(Some(view)),
                Err(message) => expansion_error.set(Some(message)),
            }
            expansion_running.set(false);
        });
    };
    let on_clear_expansion = move || {
        expansion.set(None);
        expansion_error.set(None);
        collapsed_clusters.set(Vec::new());
    };

    // Plan 44-13: StatusLine's Re-layout/Fit/Reset controls (the phase 44 UI spec Copywriting Contract).
    let on_relayout = move || {
        cosmograph.with_value(|handle| {
            if let Some(handle) = handle {
                handle.relayout();
            }
        });
    };
    // `Fit` re-fits to the WHOLE graph — `cosmograph_trait.rs` (out of this plan's file scope,
    // D-17) exposes only the indexed `fit_view_by_indices`, no whole-graph variant, so this calls
    // it with every population index rather than adding a new trait method.
    let on_fit = move || {
        let all_indices: Vec<u32> = node_ids.with_value(|ids| (0..ids.len() as u32).collect());
        cosmograph.with_value(|handle| {
            if let Some(handle) = handle {
                handle.fit_view_by_indices(&all_indices);
            }
        });
    };
    // Reset (D-12's heavier clearing action): clears both signals; the canvas's own restore to
    // raw/kind-coloured/full-opacity happens REACTIVELY via the `algorithm_result` `Effect` above
    // (its `None` branch already performs exactly the restore this control needs) — never a
    // second, independently-duplicated set of canvas calls here.
    let on_reset = move || {
        algorithm_result.set(None);
        selected_row.set(None);
        // Reset is D-12's HEAVIER clearing action, so it clears the expansion as well — otherwise
        // pressing it while expanded would leave the canvas narrowed to a neighbourhood with the
        // status line still describing it, which is not what "Reset" claims. The hop control's own
        // Clear expansion is the lighter, expansion-only action.
        expansion.set(None);
        expansion_error.set(None);
        collapsed_clusters.set(Vec::new());
    };

    // Plan 45-14 (CNV-03): everything the save sends, composed once, in `canvas_filters`' own
    // single definitions — `visible_subgraph_ids` for what is on screen and `scope_ids_for` for
    // what the filter ran over. Nothing here re-derives either. `population_ready` is read first
    // because `node_ids` is a `StoredValue` and would otherwise leave this derivation
    // unsubscribed.
    let save_payload = Signal::derive(move || {
        population_ready.get();
        let population = node_ids.with_value(|ids| ids.clone());
        let collapsed = collapsed_clusters.get();
        let view = expansion.get();
        canvas_filters::save_payload(
            view.as_ref(),
            &collapsed,
            canvas_filter_state.result.get().as_ref(),
            &population,
            canvas_filters::request_from(canvas_filter_state, Vec::new()),
        )
    });

    // Plan 45-14 (CNV-03): why the canvas is empty, when it is. Read off the SAME payload the
    // save sends, so the sentence describes the set the save would write rather than a second
    // opinion about what is on screen.
    let empty_sentence = Signal::derive(move || {
        let payload = save_payload.get();
        canvas_filters::empty_canvas_sentence(
            expansion.get().as_ref(),
            canvas_filter_state.result.get().as_ref(),
            payload.expected_node_ids.len(),
            &payload.filter,
        )
    });

    view! {
        <div>
            <Breadcrumb />
            <h1 class="text-heading">"Graph"</h1>
            <Suspense fallback=move || view! {
                <p class="text-body" style="color: var(--muted-foreground); margin-top: var(--space-lg);">"Loading graph metadata\u{2026}"</p>
            }>
                {move || {
                    match (status.get(), meta.get()) {
                        (Some(Ok(status_view)), Some(Ok(population_meta))) => {
                            let mount_canvas = should_mount_canvas(&status_view, &population_meta);
                            view! {
                                <div>
                                    {population_panel(&status_view, &population_meta)}
                                    {mount_canvas.then(move || view! {
                                        <div style="display: flex; gap: var(--space-lg); margin-top: var(--space-lg); flex-wrap: wrap;">
                                        <div style="flex: 0 0 320px; max-width: 320px; min-width: 280px; display: flex; flex-direction: column; gap: var(--space-md);">
                                            <ToolRail state=tool_rail_state />
                                            <CanvasFilters state=canvas_filter_state />
                                            <SaveVisibleSubgraph payload=save_payload />
                                            <ResultsList result=algorithm_result.into() selected_row=selected_row />
                                        </div>
                                        <div style="position: sticky; top: 56px; align-self: flex-start; height: calc(100vh - 56px); flex: 1 1 480px; min-width: 320px; display: flex; flex-direction: column;">
                                            {move || match population_arrow.get() {
                                                None => view! {
                                                    <p class="text-body" style="color: var(--muted-foreground);">{CANVAS_LOADING_COPY}</p>
                                                }.into_any(),
                                                // A `503` means the CSR bake has not landed. That is
                                                // not a failure the operator can act on, so it gets
                                                // the neutral baking copy and no Retry button — the
                                                // effect below re-requests it on its own.
                                                Some(Err(PopulationFetchError::Baking)) => view! {
                                                    <p class="text-body" style="color: var(--muted-foreground);">
                                                        {POPULATION_ARROW_BAKING_COPY}
                                                    </p>
                                                }.into_any(),
                                                Some(Err(PopulationFetchError::Unreadable)) => view! {
                                                    <div style="display: flex; flex-direction: column; gap: var(--space-sm); align-items: flex-start;">
                                                        <p class="text-body" style="color: var(--destructive); margin: 0;">
                                                            {POPULATION_ARROW_FETCH_ERROR_COPY}
                                                        </p>
                                                        <button
                                                            class="text-body"
                                                            style="background: var(--primary); color: var(--primary-foreground); border: none; border-radius: 6px; padding: var(--space-xs) var(--space-md); cursor: pointer; min-height: 44px;"
                                                            on:click=move |_| { population_arrow.refetch(); }
                                                        >
                                                            "Retry"
                                                        </button>
                                                    </div>
                                                }.into_any(),
                                                Some(Ok(_)) => view! {
                                                    <div style="flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column;">
                                                        <HopControl
                                                            seed_label=seed_label
                                                            hops=expansion_hops
                                                            running=expansion_running.into()
                                                            expanded=Signal::derive(move || expansion.get().is_some())
                                                            on_expand=on_expand
                                                            on_clear=on_clear_expansion
                                                        />
                                                        <ClusterGlyphs
                                                            expansion=expansion.into()
                                                            collapsed=collapsed_clusters
                                                        />
                                                        {move || expansion_error.get().map(|message| view! {
                                                            <p class="text-body" role="alert" style="margin: 0 0 var(--space-sm) 0; color: var(--destructive);">
                                                                {message}
                                                            </p>
                                                        })}
                                                        {move || canvas_error.get().map(|message| view! {
                                                            <p
                                                                class="text-body"
                                                                role="alert"
                                                                style="position: absolute; top: var(--space-md); left: var(--space-md); right: var(--space-md); margin: 0; padding: var(--space-sm) var(--space-md); background: var(--card); border: 1px solid var(--border); border-radius: 8px; color: var(--foreground); z-index: 1;"
                                                            >
                                                                {message}
                                                            </p>
                                                        })}
                                                        <div style="flex: 1 1 auto; min-height: 0; position: relative; display: flex; flex-direction: column;">
                                                            <div
                                                                node_ref=canvas_ref
                                                                style="flex: 1 1 auto; min-height: 0; background: var(--background);"
                                                            ></div>
                                                            // An empty canvas SAYS WHY. A blank
                                                            // rectangle lets "the filters excluded
                                                            // everything" be read as "this entity
                                                            // has no neighbours", which are
                                                            // completely different facts.
                                                            {move || empty_sentence.get().map(|sentence| view! {
                                                                <p
                                                                    class="text-body"
                                                                    role="status"
                                                                    style="position: absolute; top: 50%; left: var(--space-lg); right: var(--space-lg); transform: translateY(-50%); margin: 0; padding: var(--space-md); background: var(--card); border: 1px solid var(--border); border-radius: 8px; color: var(--foreground); text-align: center;"
                                                                >
                                                                    {sentence}
                                                                </p>
                                                            })}
                                                        </div>
                                                        {move || {
                                                            clicked_index.get().and_then(|idx| {
                                                                node_ids.with_value(|ids| ids.get(idx as usize).cloned())
                                                            }).map(|id| {
                                                                let close = move || clicked_index.set(None);
                                                                // How the current expansion reached
                                                                // THIS node, if it did — `None` for
                                                                // the seed, for a node outside the
                                                                // expansion and when none is running.
                                                                let id_for_reach = id.clone();
                                                                let reached = Signal::derive(move || {
                                                                    expansion.get().as_ref().and_then(|view| {
                                                                        neighbourhood::reached_node(view, &id_for_reach).cloned()
                                                                    })
                                                                });
                                                                view! { <SideCard node_id=id reached=reached on_close=close /> }
                                                            })
                                                        }}
                                                        <StatusLine
                                                            result=algorithm_result.into()
                                                            expansion=expansion.into()
                                                            filter=canvas_filter_state.result.into()
                                                            from_ref=from_ref.into()
                                                            to_ref=to_ref.into()
                                                            on_relayout=on_relayout
                                                            on_fit=on_fit
                                                            on_reset=on_reset
                                                        />
                                                    </div>
                                                }.into_any(),
                                            }}
                                        </div>
                                        </div>
                                    })}
                                </div>
                            }.into_any()
                        }
                        (Some(_), Some(_)) => view! {
                            <p class="text-body" style="color: var(--muted-foreground); margin-top: var(--space-lg);">
                                {GRAPH_FETCH_ERROR_COPY}
                            </p>
                        }.into_any(),
                        _ => ().into_any(),
                    }
                }}
            </Suspense>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::PopulationView;

    fn population(count: Option<u64>, unavailable_reason: Option<&str>) -> PopulationMeta {
        PopulationMeta {
            count,
            generated_at: "2026-01-01T10:30:00Z".to_string(),
            unavailable_reason: unavailable_reason.map(|s| s.to_string()),
            ward_seed_count: 14,
        }
    }

    fn status(node_count: u64, edge_count: u64, snapshot_pending: bool) -> StatusView {
        StatusView {
            analysis_name: "First analysis".to_string(),
            database: "an_first_1a2b3c".to_string(),
            node_count,
            edge_count,
            graph_as_of: "2026-01-01T10:33:00Z".to_string(),
            population: PopulationView {
                count: Some(113_250),
                generated_at: "2026-01-01T10:30:00Z".to_string(),
                unavailable_reason: None,
            },
            ingestors_running: false,
            actor: "operator".to_string(),
            snapshot_pending,
        }
    }

    #[test]
    fn population_meta_url_matches_the_native_endpoint() {
        assert_eq!(POPULATION_META_URL, "/api/graph/population/meta");
    }

    #[test]
    fn graph_fetch_error_copy_contains_the_endpoint_path() {
        assert!(GRAPH_FETCH_ERROR_COPY.contains("/api/graph/population/meta"));
    }

    #[test]
    fn population_panel_headline_renders_the_plain_count_when_available() {
        let meta = population(Some(113_250), None);
        assert_eq!(population_panel_headline(&meta), "113250");
    }

    #[test]
    fn population_panel_headline_never_returns_a_fabricated_zero_when_unavailable() {
        let meta = population(None, Some("no ward code resolved"));
        assert_eq!(population_panel_headline(&meta), "Population unavailable");
        assert_ne!(population_panel_headline(&meta), "0");
    }

    /// Checkpoint fix item 1: the ward-seed caption is a stable, non-empty label — never a bare
    /// number with no caption beside it.
    #[test]
    fn ward_seed_caption_is_a_stable_non_empty_label() {
        assert_eq!(WARD_SEED_CAPTION, "ward seeds");
        assert!(!WARD_SEED_CAPTION.is_empty());
    }

    /// Checkpoint fix item 4: [`should_show_snapshot_baking`] reflects `StatusView.snapshot_pending`
    /// exactly — the pure branch-selection logic `population_panel` dispatches on.
    #[test]
    fn should_show_snapshot_baking_reflects_the_status_flag() {
        assert!(should_show_snapshot_baking(&status(0, 0, true)));
        assert!(!should_show_snapshot_baking(&status(
            358_484, 507_179, false
        )));
    }

    /// [`SNAPSHOT_BAKING_HEADING`] is a stable, non-empty, honest label — never blank and never
    /// the literal fabricated-zero shape ("0 nodes").
    #[test]
    fn snapshot_baking_heading_is_stable_and_never_a_fabricated_zero() {
        assert!(!SNAPSHOT_BAKING_HEADING.is_empty());
        assert!(!SNAPSHOT_BAKING_HEADING.contains('0'));
    }

    #[test]
    fn population_arrow_url_matches_the_native_endpoint() {
        assert_eq!(POPULATION_ARROW_URL, "/api/graph/population");
    }

    #[test]
    fn population_arrow_fetch_error_copy_names_the_arrow_endpoint() {
        assert!(POPULATION_ARROW_FETCH_ERROR_COPY.contains("/api/graph/population"));
        assert!(!POPULATION_ARROW_FETCH_ERROR_COPY.contains("/api/graph/population/meta"));
    }

    /// QA-gate defect log 2026-09-13 defect 1: `population_handler` answers `503` for as long as
    /// the CSR bake has not landed, and the page reported that as unreadable data. `503` — and
    /// ONLY `503` — must classify as the self-healing baking state.
    #[test]
    fn a_503_is_the_baking_state_and_every_other_status_is_unreadable() {
        assert_eq!(
            population_error_for_status(503),
            PopulationFetchError::Baking
        );
        for status in [200, 204, 400, 401, 403, 404, 429, 500, 502, 504] {
            assert_eq!(
                population_error_for_status(status),
                PopulationFetchError::Unreadable,
                "status {status} must not be treated as a baking snapshot"
            );
        }
    }

    /// The two states must not share copy — the whole defect was one message standing in for
    /// both. The baking copy must also not accuse the workbench of returning bad data, and must
    /// not offer the endpoint path that belongs to the error state's own wording.
    #[test]
    fn the_baking_copy_is_distinct_from_the_unreadable_copy_and_does_not_allege_bad_data() {
        assert_ne!(
            POPULATION_ARROW_BAKING_COPY,
            POPULATION_ARROW_FETCH_ERROR_COPY
        );
        assert!(!POPULATION_ARROW_BAKING_COPY.is_empty());
        assert!(!POPULATION_ARROW_BAKING_COPY.contains("cannot read"));
        assert!(!POPULATION_ARROW_BAKING_COPY.contains("/api/graph/population"));
    }

    #[test]
    fn canvas_loading_copy_is_the_exact_ui_spec_string() {
        assert_eq!(CANVAS_LOADING_COPY, "Loading the graph\u{2026}");
    }

    #[test]
    fn warm_up_duration_is_three_seconds() {
        assert_eq!(WARM_UP_DURATION, Duration::from_secs(3));
    }

    #[test]
    fn should_mount_canvas_is_true_only_when_available_and_not_baking() {
        let available_meta = population(Some(113_250), None);
        let unavailable_meta = population(None, Some("no ward code resolved"));

        assert!(should_mount_canvas(
            &status(358_484, 507_179, false),
            &available_meta
        ));
        assert!(
            !should_mount_canvas(&status(0, 0, true), &available_meta),
            "baking must supersede an otherwise-available population"
        );
        assert!(!should_mount_canvas(
            &status(358_484, 507_179, false),
            &unavailable_meta
        ));
    }

    // ─── split_population_arrow ────────────────────────────────────────────────────────────

    fn framed(nodes: &[u8], links: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
        out.extend_from_slice(nodes);
        out.extend_from_slice(&(links.len() as u32).to_le_bytes());
        out.extend_from_slice(links);
        out
    }

    /// The bytes `arrow::ipc::writer::StreamWriter` ACTUALLY produces, captured from a live
    /// `GET /api/graph/population` against the planted-fixture analysis (43 reachable nodes, 81
    /// links — re-measured against the extended fixture, Phase 50 plan 10). Every other Arrow
    /// test in this module hand-builds its fixture, and a hand-built fixture only exercises the
    /// encoder the test author imagined: all of them wrote `Message.bodyLength` explicitly, so
    /// none reached the absent-field path that real arrow output takes on its schema message, and
    /// `parse_utf8_column` returned an empty `Vec` for every real stream while the suite stayed
    /// green.
    const REAL_POPULATION_ARROW: &[u8] = include_bytes!("testdata/population-real-arrow.bin");

    /// PROVE THIS CAN FAIL: before the absent-`bodyLength` fix in [`read_message_frame`], this
    /// returned 0 ids, not 43 — the whole canvas-effect and side-card wiring was dead because of
    /// it.
    #[test]
    fn real_arrow_bytes_parse_into_the_full_population() {
        let (nodes, links) =
            split_population_arrow(REAL_POPULATION_ARROW).expect("real payload must split");
        assert!(!nodes.is_empty() && !links.is_empty());

        let ids = parse_utf8_column(nodes, 0);
        assert_eq!(ids.len(), 43, "every reachable node id must parse");
        assert!(
            ids.contains(&"ward:X05999001".to_string()),
            "the ward seed must be among the parsed ids, got first 3: {:?}",
            &ids[..3]
        );

        let names = parse_utf8_column(nodes, 2);
        assert_eq!(
            names.len(),
            43,
            "the name column must parse to the same row count"
        );
        assert!(
            names.iter().any(|n| n == "Planted Bridge Officer"),
            "a known planted display name must round-trip"
        );

        // Row order is the contract the canvas index mapping depends on: index i's point is
        // `node_ids[i]`, so ids and names must line up row for row.
        let ward_row = ids.iter().position(|i| i == "ward:X05999001").unwrap();
        assert_eq!(
            names[ward_row], "Planted Ward",
            "columns must stay row-aligned"
        );
    }

    /// The exact FlatBuffers semantic the bug turned on, isolated from Arrow: a field omitted
    /// from the vtable carries its DEFAULT, and `Message.bodyLength`'s default is 0. Guards the
    /// fix directly, with no dependency on the committed byte fixture.
    #[test]
    fn a_message_with_no_body_length_field_reads_as_body_len_zero() {
        // A `Message` table whose vtable declares only 3 fields, so field index 3
        // (`bodyLength`) is past its end — exactly how arrow-rs encodes a schema message.
        let mut meta = vec![0u8; 4];
        // vtable: vt_len = 4 + 3*2 = 10, table_len = 8, three present fields.
        let vtable_pos = meta.len();
        meta.extend_from_slice(&10u16.to_le_bytes()); // vtable length
        meta.extend_from_slice(&8u16.to_le_bytes()); // table length
        meta.extend_from_slice(&8u16.to_le_bytes()); // field 0
        meta.extend_from_slice(&7u16.to_le_bytes()); // field 1
        meta.extend_from_slice(&4u16.to_le_bytes()); // field 2
        let table_pos = meta.len();
        let soffset = table_pos as i32 - vtable_pos as i32;
        meta.extend_from_slice(&soffset.to_le_bytes());
        meta.extend_from_slice(&[0u8; 4]); // field 2 payload placeholder
        write_u32_le_at(&mut meta, 0, table_pos as u32);

        let mut stream = Vec::new();
        stream.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        stream.extend_from_slice(&(meta.len() as i32).to_le_bytes());
        stream.extend_from_slice(&meta);

        let (_, meta_len, _, body_len, next) =
            read_message_frame(&stream, 0).expect("an absent bodyLength must not fail the frame");
        assert_eq!(body_len, 0, "absent bodyLength must read as its default, 0");
        assert_eq!(
            next,
            8 + meta_len,
            "a body-less message ends where its metadata does"
        );
    }

    #[test]
    fn split_population_arrow_recovers_both_streams() {
        let bytes = framed(b"nodes-stream-bytes", b"links-stream-bytes-longer");
        let (nodes, links) = split_population_arrow(&bytes).expect("must split");
        assert_eq!(nodes, b"nodes-stream-bytes");
        assert_eq!(links, b"links-stream-bytes-longer");
    }

    #[test]
    fn split_population_arrow_handles_empty_streams() {
        let bytes = framed(b"", b"");
        let (nodes, links) = split_population_arrow(&bytes).expect("must split");
        assert!(nodes.is_empty());
        assert!(links.is_empty());
    }

    /// PROVE THIS CAN FAIL: truncate the buffer mid-way through the links length prefix — a
    /// naive slicer that trusts the declared length without bounds-checking would panic (index
    /// out of range) instead of returning `None`.
    #[test]
    fn split_population_arrow_returns_none_never_panics_on_truncated_input() {
        assert_eq!(split_population_arrow(&[]), None);
        assert_eq!(split_population_arrow(&[1, 2, 3]), None);
        let mut bytes = framed(b"nodes", b"links");
        bytes.truncate(bytes.len() - 1);
        assert_eq!(split_population_arrow(&bytes), None);
        // A declared nodes length far larger than the buffer itself.
        let mut malicious = Vec::new();
        malicious.extend_from_slice(&u32::MAX.to_le_bytes());
        malicious.extend_from_slice(b"short");
        assert_eq!(split_population_arrow(&malicious), None);
    }

    // ─── parse_utf8_column (hand-built FlatBuffers/Arrow IPC fixtures) ────────────────────

    fn write_u32_le_at(buf: &mut [u8], pos: usize, value: u32) {
        buf[pos..pos + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn write_i32_le_at(buf: &mut [u8], pos: usize, value: i32) {
        buf[pos..pos + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn write_u16_le_at(buf: &mut [u8], pos: usize, value: u16) {
        buf[pos..pos + 2].copy_from_slice(&value.to_le_bytes());
    }

    /// Hand-builds a minimal, spec-correct FlatBuffers-encoded `Message` table whose ONLY set
    /// field is `bodyLength` (field index 3) — sufficient for [`read_message_frame`] to parse
    /// successfully without a real `Schema` table, mirroring how [`parse_utf8_column_checked`]
    /// never actually inspects message 1's content.
    fn encode_bodylength_only_message(body_length: i64) -> Vec<u8> {
        let mut buf = vec![0u8; 4]; // root uoffset placeholder, patched below.

        let vtable_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 12]); // fields 0..3 => 4 slots => size 4 + 4*2 = 12.
        write_u16_le_at(&mut buf, vtable_pos as usize, 12);
        write_u16_le_at(&mut buf, vtable_pos as usize + 2, 12);

        let table_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 4]); // soffset to vtable, patched below.
        let field3_pos = buf.len() as u32;
        buf.extend_from_slice(&body_length.to_le_bytes());

        write_i32_le_at(
            &mut buf,
            table_pos as usize,
            (table_pos - vtable_pos) as i32,
        );
        write_u16_le_at(
            &mut buf,
            vtable_pos as usize + 4 + 3 * 2,
            (field3_pos - table_pos) as u16,
        );
        write_u32_le_at(&mut buf, 0, table_pos);

        buf
    }

    /// Hand-builds a `Message` table whose `header` (field 2) points at a `RecordBatch` table
    /// whose `buffers` (field 2) points at the given `(offset, length)` `Buffer` structs, and
    /// whose `bodyLength` (field 3) is set — the shape [`parse_utf8_column_checked`] actually
    /// reads for the record-batch message. Structures are written in forward-reference order
    /// (Message vtable+table first, then RecordBatch vtable+table, then the buffers vector) so
    /// every `uoffset_t` computed as `target - field_pos` is positive, matching
    /// [`fb_indirect`]'s unsigned-addition read convention.
    fn encode_record_batch_message(buffers: &[(i64, i64)], body_length: i64) -> Vec<u8> {
        let mut buf = vec![0u8; 4]; // root uoffset placeholder.

        let msg_vtable_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 12]);
        write_u16_le_at(&mut buf, msg_vtable_pos as usize, 12);
        write_u16_le_at(&mut buf, msg_vtable_pos as usize + 2, 12);

        let msg_table_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 4]); // soffset placeholder.
        let msg_field2_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 4]); // header uoffset placeholder, patched after RecordBatch.
        let msg_field3_pos = buf.len() as u32;
        buf.extend_from_slice(&body_length.to_le_bytes());

        write_i32_le_at(
            &mut buf,
            msg_table_pos as usize,
            (msg_table_pos - msg_vtable_pos) as i32,
        );
        write_u16_le_at(
            &mut buf,
            msg_vtable_pos as usize + 4 + 2 * 2,
            (msg_field2_pos - msg_table_pos) as u16,
        );
        write_u16_le_at(
            &mut buf,
            msg_vtable_pos as usize + 4 + 3 * 2,
            (msg_field3_pos - msg_table_pos) as u16,
        );
        write_u32_le_at(&mut buf, 0, msg_table_pos);

        let rb_vtable_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 10]); // fields 0,1,2 => 3 slots => size 4 + 3*2 = 10.
        write_u16_le_at(&mut buf, rb_vtable_pos as usize, 10);
        write_u16_le_at(&mut buf, rb_vtable_pos as usize + 2, 8);

        let rb_table_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 4]); // soffset placeholder.
        let rb_field2_pos = buf.len() as u32;
        buf.extend_from_slice(&[0u8; 4]); // buffers uoffset placeholder.

        write_i32_le_at(
            &mut buf,
            rb_table_pos as usize,
            (rb_table_pos - rb_vtable_pos) as i32,
        );
        write_u16_le_at(
            &mut buf,
            rb_vtable_pos as usize + 4 + 2 * 2,
            (rb_field2_pos - rb_table_pos) as u16,
        );

        // Patch Message.field2 (header) to point forward at the RecordBatch table.
        write_u32_le_at(
            &mut buf,
            msg_field2_pos as usize,
            rb_table_pos - msg_field2_pos,
        );

        let buffers_vec_pos = buf.len() as u32;
        buf.extend_from_slice(&(buffers.len() as u32).to_le_bytes());
        for (offset, length) in buffers {
            buf.extend_from_slice(&offset.to_le_bytes());
            buf.extend_from_slice(&length.to_le_bytes());
        }
        write_u32_le_at(
            &mut buf,
            rb_field2_pos as usize,
            buffers_vec_pos - rb_field2_pos,
        );

        buf
    }

    fn pad8(mut v: Vec<u8>) -> Vec<u8> {
        while !v.len().is_multiple_of(8) {
            v.push(0);
        }
        v
    }

    fn wrap_message(metadata: Vec<u8>, body: &[u8]) -> Vec<u8> {
        let metadata = pad8(metadata);
        let mut out = Vec::new();
        out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        out.extend_from_slice(&(metadata.len() as i32).to_le_bytes());
        out.extend_from_slice(&metadata);
        out.extend_from_slice(&pad8(body.to_vec()));
        out
    }

    /// Builds a full test stream for a 3-`Utf8`-column schema (mirroring `arrow_export.rs`'s
    /// `id`/`kind`/`name` nodes-stream shape), so `column_index=0` genuinely exercises the
    /// per-column 3-buffer stride math, not a degenerate 1-column special case.
    fn build_fixture_stream(
        id_values: &[&str],
        kind_values: &[&str],
        name_values: &[&str],
    ) -> Vec<u8> {
        assert_eq!(id_values.len(), kind_values.len());
        assert_eq!(id_values.len(), name_values.len());

        fn column_buffers(values: &[&str]) -> (Vec<u8>, Vec<u8>) {
            let mut offsets = Vec::new();
            let mut data = Vec::new();
            offsets.extend_from_slice(&0u32.to_le_bytes());
            for v in values {
                data.extend_from_slice(v.as_bytes());
                offsets.extend_from_slice(&(data.len() as u32).to_le_bytes());
            }
            (offsets, data)
        }

        let columns = [
            column_buffers(id_values),
            column_buffers(kind_values),
            column_buffers(name_values),
        ];

        let mut body = Vec::new();
        let mut buffers = Vec::new();
        for (offsets, data) in &columns {
            buffers.push((body.len() as i64, 0i64)); // validity: empty (non-nullable columns).
            buffers.push((body.len() as i64, offsets.len() as i64));
            body.extend_from_slice(offsets);
            while !body.len().is_multiple_of(8) {
                body.push(0);
            }
            buffers.push((body.len() as i64, data.len() as i64));
            body.extend_from_slice(data);
            while !body.len().is_multiple_of(8) {
                body.push(0);
            }
        }

        let schema_message = wrap_message(encode_bodylength_only_message(0), &[]);
        let record_batch_message = wrap_message(
            encode_record_batch_message(&buffers, body.len() as i64),
            &body,
        );

        let mut stream = Vec::new();
        stream.extend_from_slice(&schema_message);
        stream.extend_from_slice(&record_batch_message);
        stream
    }

    #[test]
    fn parses_column_zero_id_values_from_a_hand_built_three_column_stream() {
        let stream = build_fixture_stream(
            &["director:jane-smith", "ward:X05001499", "GB-COH:99000001"],
            &["Entity", "Ward", "Entity"],
            &["Jane Smith", "Sample Ward", "Acme Ltd"],
        );
        let ids = parse_utf8_column(&stream, 0);
        assert_eq!(
            ids,
            vec!["director:jane-smith", "ward:X05001499", "GB-COH:99000001"]
        );
    }

    /// PROVE THIS CAN FAIL: proves the per-column buffer STRIDE, not merely column 0 — a reader
    /// that always read buffer indices 1/2 (ignoring `column_index`) would pass the id-column
    /// test above but fail this one.
    #[test]
    fn parses_column_two_name_values_proving_the_per_column_buffer_stride() {
        let stream =
            build_fixture_stream(&["a", "b"], &["Entity", "Officer"], &["Alpha Ltd", "Bravo"]);
        let names = parse_utf8_column(&stream, 2);
        assert_eq!(names, vec!["Alpha Ltd", "Bravo"]);
    }

    #[test]
    fn parses_kind_column_one() {
        let stream =
            build_fixture_stream(&["a", "b"], &["Entity", "Officer"], &["Alpha Ltd", "Bravo"]);
        let kinds = parse_utf8_column(&stream, 1);
        assert_eq!(kinds, vec!["Entity", "Officer"]);
    }

    #[test]
    fn parses_a_single_row_stream() {
        let stream = build_fixture_stream(&["only"], &["Entity"], &["Only Ltd"]);
        assert_eq!(parse_utf8_column(&stream, 0), vec!["only"]);
    }

    #[test]
    fn parses_an_empty_stream_with_no_rows() {
        let stream = build_fixture_stream(&[], &[], &[]);
        assert_eq!(parse_utf8_column(&stream, 0), Vec::<String>::new());
    }

    /// PROVE THIS CAN FAIL: malformed/truncated/garbage input must degrade to an empty `Vec`,
    /// never panic — the honest-degradation behaviour a side card depends on when a clicked
    /// point's id cannot be resolved.
    #[test]
    fn malformed_bytes_return_an_empty_vec_never_panic() {
        assert_eq!(parse_utf8_column(&[], 0), Vec::<String>::new());
        assert_eq!(parse_utf8_column(&[1, 2, 3], 0), Vec::<String>::new());
        assert_eq!(
            parse_utf8_column(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0], 0),
            Vec::<String>::new()
        );
        let stream = build_fixture_stream(&["a"], &["Entity"], &["Alpha"]);
        // A column index far beyond the 3 real columns.
        assert_eq!(parse_utf8_column(&stream, 99), Vec::<String>::new());
    }

    // ─── resolve_indices / automatic_hidden_ids / selected_row_node_ids (plan 44-13 Task 3) ──

    use urban_integrity_workbench_dto::{CommunityResult, PathHop, PathResult, RankedEntity};

    fn entity(id: &str, name: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Entity".to_string(),
        }
    }

    #[test]
    fn resolve_indices_maps_known_ids_and_skips_unknown_ones() {
        let mut map = HashMap::new();
        map.insert("a".to_string(), 0u32);
        map.insert("b".to_string(), 5u32);
        let ids = vec!["a".to_string(), "unknown".to_string(), "b".to_string()];
        assert_eq!(resolve_indices(&ids, &map), vec![0, 5]);
    }

    #[test]
    fn automatic_hidden_ids_for_community_and_k_core_use_hidden_node_ids_directly() {
        let community_outcome = AlgorithmOutcome::Community {
            communities: vec![],
            hidden_node_ids: vec!["h1".to_string(), "h2".to_string()],
        };
        assert_eq!(
            automatic_hidden_ids(&community_outcome, &[]),
            vec!["h1".to_string(), "h2".to_string()]
        );

        let k_core_outcome = AlgorithmOutcome::KCore {
            k: 3,
            node_count: 1,
            edge_count: 1,
            top_nodes: vec![],
            hidden_node_ids: vec!["h3".to_string()],
        };
        assert_eq!(
            automatic_hidden_ids(&k_core_outcome, &[]),
            vec!["h3".to_string()]
        );
    }

    #[test]
    fn automatic_hidden_ids_for_ward_subgraph_is_population_minus_included() {
        let outcome = AlgorithmOutcome::WardSubgraph {
            ward: entity("ward:X05001499", "Sample Ward"),
            hops: 2,
            node_count: 2,
            edge_count: 1,
            node_ids: vec!["a".to_string(), "b".to_string()],
        };
        let population = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut hidden = automatic_hidden_ids(&outcome, &population);
        hidden.sort();
        assert_eq!(hidden, vec!["c".to_string(), "d".to_string()]);
    }

    #[test]
    fn automatic_hidden_ids_for_betweenness_and_paths_is_always_empty() {
        let betweenness = AlgorithmOutcome::Betweenness {
            sampled_sources: 1,
            total_sources: 1,
            top_nodes: vec![],
        };
        assert_eq!(
            automatic_hidden_ids(&betweenness, &["a".to_string()]),
            Vec::<String>::new()
        );
        let paths = AlgorithmOutcome::Paths { paths: vec![] };
        assert_eq!(
            automatic_hidden_ids(&paths, &["a".to_string()]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn selected_row_node_ids_for_paths_returns_that_paths_hop_ids() {
        let outcome = AlgorithmOutcome::Paths {
            paths: vec![
                PathResult {
                    hops: vec![
                        PathHop {
                            entity: entity("a", "A"),
                            edge_type_to_next: Some("Directs".to_string()),
                        },
                        PathHop {
                            entity: entity("b", "B"),
                            edge_type_to_next: None,
                        },
                    ],
                },
                PathResult {
                    hops: vec![PathHop {
                        entity: entity("c", "C"),
                        edge_type_to_next: None,
                    }],
                },
            ],
        };
        assert_eq!(
            selected_row_node_ids(&outcome, 0, &[]),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            selected_row_node_ids(&outcome, 1, &[]),
            vec!["c".to_string()]
        );
        assert_eq!(
            selected_row_node_ids(&outcome, 5, &[]),
            Vec::<String>::new(),
            "an out-of-range row index degrades to no indices, never panics"
        );
    }

    #[test]
    fn selected_row_node_ids_for_community_returns_the_full_member_list() {
        // top_entities (2) is deliberately smaller than member_node_ids (5) — proves the
        // row-selection highlight covers the WHOLE community, not just the capped display list
        // (44-17 closed 44-13's Deviation 4 gap: CommunityResult now carries complete membership).
        let outcome = AlgorithmOutcome::Community {
            communities: vec![CommunityResult {
                index: 0,
                size: 5,
                top_entities: vec![entity("a", "A"), entity("b", "B")],
                color_hue_deg: 90,
                member_node_ids: vec![
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                    "d".to_string(),
                    "e".to_string(),
                ],
            }],
            hidden_node_ids: vec![],
        };
        assert_eq!(
            selected_row_node_ids(&outcome, 0, &[]),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
            ],
            "must return the complete member_node_ids list, not just top_entities"
        );
    }

    #[test]
    fn selected_row_node_ids_for_k_core_summary_row_is_population_minus_hidden() {
        let outcome = AlgorithmOutcome::KCore {
            k: 3,
            node_count: 3,
            edge_count: 2,
            top_nodes: vec![RankedEntity {
                entity: entity("a", "A"),
                score: 2.0,
            }],
            hidden_node_ids: vec!["z".to_string()],
        };
        let population = vec!["a".to_string(), "b".to_string(), "z".to_string()];
        let mut ids = selected_row_node_ids(&outcome, 0, &population);
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn selected_row_node_ids_for_k_core_node_row_is_the_single_ranked_entity() {
        let outcome = AlgorithmOutcome::KCore {
            k: 3,
            node_count: 3,
            edge_count: 2,
            top_nodes: vec![
                RankedEntity {
                    entity: entity("a", "A"),
                    score: 2.0,
                },
                RankedEntity {
                    entity: entity("b", "B"),
                    score: 1.0,
                },
            ],
            hidden_node_ids: vec![],
        };
        assert_eq!(
            selected_row_node_ids(&outcome, 1, &[]),
            vec!["a".to_string()]
        );
        assert_eq!(
            selected_row_node_ids(&outcome, 2, &[]),
            vec!["b".to_string()]
        );
    }

    #[test]
    fn selected_row_node_ids_for_betweenness_is_the_single_ranked_entity() {
        let outcome = AlgorithmOutcome::Betweenness {
            sampled_sources: 1,
            total_sources: 1,
            top_nodes: vec![RankedEntity {
                entity: entity("a", "A"),
                score: 5.0,
            }],
        };
        assert_eq!(
            selected_row_node_ids(&outcome, 0, &[]),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn selected_row_node_ids_for_ward_subgraph_is_the_full_induced_set() {
        let outcome = AlgorithmOutcome::WardSubgraph {
            ward: entity("ward:X05001499", "Sample Ward"),
            hops: 2,
            node_count: 2,
            edge_count: 1,
            node_ids: vec!["a".to_string(), "b".to_string()],
        };
        assert_eq!(
            selected_row_node_ids(&outcome, 0, &[]),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
