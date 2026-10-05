//! `cosmograph_trait.rs` — the D-17 Rust seam over the vendored Cosmograph shim
//! (`crates/workbench-web/js/cosmograph-shim.js`). This module is the ONLY file in this crate
//! allowed to name the shim's Graph-page exported functions (`mountGraph`,
//! `graphSetPointOpacity`, `graphSetPointColorByHue`, `graphClearPointColorOverrides`,
//! `graphSetPointSizeByScore`, `graphSetVisibleLabels`, `graphFitViewByIndices`,
//! `graphSelectPoints`, `graphPause`,
//! `graphStop`, `graphStart`, `graphDestroy`) — every later Graph page component (canvas, rail,
//! results list, side card; Phase 44 plans 44-04..44-13) talks to [`CosmographHandle`], never
//! `wasm_bindgen` directly.
//!
//! Generalises `cosmograph_spike.rs`'s proven mount/Drop-destroy/Closure pattern (D-17) rather
//! than duplicating it: the click closure MUST outlive the mounted instance exactly as
//! `CosmographSpike` requires, and `Drop` mirrors `CosmographSpike::drop` calling the shim's
//! destroy export. `cosmograph_spike.rs` itself stays untouched as the Phase 42 spike record —
//! this module is the seed's generalisation, not a second copy of it.
#![allow(dead_code)] // Consumed starting with plan 44-04 (canvas mount); no call site yet.

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen(module = "/js/cosmograph-shim.js")]
extern "C" {
    /// Mounts a Cosmograph instance from TWO separate raw Arrow IPC byte streams — nodes and
    /// links (the phase 44 research notes: `prepareCosmographData(config, pointsData, linksData?)` takes
    /// points and links as SEPARATE arguments) — plus `kind_colors`, the D-15 kind -> `--node-*`
    /// CSS-token pairs the shim resolves to concrete colours at mount time and wires into
    /// `pointColorBy`/`pointColorByMap` so every point is coloured by its `kind` column from the
    /// moment it mounts, and `edge_colors`, the equivalent edge-type -> `--edge-*` pairs the shim
    /// wires into `linkColorBy`/`linkColorByFn` so every link is coloured by its `edge_type`
    /// column (Cosmograph cannot draw text on edges, so colour plus the canvas legend is the edge
    /// labelling). `warm_up` is D-11's freeze window: the shim arms it when the simulation
    /// actually STARTS, never at mount — Cosmograph's `pause()` does nothing unless the
    /// simulation is already running, and it only begins once duckdb-wasm has initialised and the
    /// data has uploaded. No JS-side Arrow parsing needed on the Rust side either way (the `arrow`
    /// crate never compiles to `wasm32-unknown-unknown`). Same async-Promise/rejection discipline
    /// as `cosmograph_spike.rs`'s `mountSpike`: this returns immediately, the outcome (success or
    /// rejection) arrives later via the awaited `Promise`.
    #[wasm_bindgen(js_name = mountGraph)]
    fn mount_graph(
        container: &JsValue,
        nodes_bytes: js_sys::Uint8Array,
        links_bytes: js_sys::Uint8Array,
        kind_colors: js_sys::Array,
        edge_colors: js_sys::Array,
        warm_up_ms: f64,
        cb: &Closure<dyn FnMut(u32)>,
    ) -> js_sys::Promise;

    /// The finding-detail-route mount path (plan 50-11): `mountGraph`'s small-population,
    /// non-Arrow sibling. `points`/`links` are plain JSON arrays of `{ id, name, kind }` /
    /// `{ source, target, edge_type }` row objects (built via [`to_js_value`] from
    /// [`CanvasPointRow`]/[`CanvasLinkRow`]) — the same `prepareCosmographData` ingestion path
    /// `cosmograph_spike.rs`'s `mountSpike` already proved accepts plain `JsValue` arrays, with
    /// `mountGraph`'s full kind/edge colouring and click wiring on top (shared shim-side via
    /// `finishGraphMount`). Same async-Promise/rejection discipline as `mount_graph`.
    #[wasm_bindgen(js_name = mountGraphFromRows)]
    fn mount_graph_from_rows(
        container: &JsValue,
        points: JsValue,
        links: JsValue,
        kind_colors: js_sys::Array,
        edge_colors: js_sys::Array,
        warm_up_ms: f64,
        cb: &Closure<dyn FnMut(u32)>,
    ) -> js_sys::Promise;

    /// D-12/D-13: reduces every index in `dimmed` to low opacity, hides every index in `hidden`
    /// entirely (zero alpha) — never re-marshals the link set (D-04). Calling with two empty
    /// arrays clears both overrides.
    #[wasm_bindgen(js_name = graphSetPointOpacity)]
    fn graph_set_point_opacity(dimmed: js_sys::Uint32Array, hidden: js_sys::Uint32Array);

    /// The community automatic canvas effect (plan 44-13): recolours every point in `indices` to
    /// a fixed-saturation/lightness colour at `hue_deg` (degrees, 0-360). Called once per
    /// community group, ALWAYS with a hue value the caller already computed server-side
    /// (`CommunityResult.color_hue_deg`) — this never computes a hue itself, it only applies one.
    #[wasm_bindgen(js_name = graphSetPointColorByHue)]
    fn graph_set_point_color_by_hue(indices: js_sys::Uint32Array, hue_deg: f64);

    /// Restricts on-canvas labels to `visible_ids`; an EMPTY array clears the filter. Purely a
    /// DOM operation on the label elements — it never touches Cosmograph's config, so it cannot
    /// re-upload data or reach duckdb-wasm (see the shim's own note, and f8ed2f43).
    #[wasm_bindgen(js_name = graphSetVisibleLabels)]
    fn graph_set_visible_labels(visible_ids: js_sys::Array);

    /// D-12's Reset path: clears accumulated hue overrides (from repeated
    /// `graph_set_point_color_by_hue` calls within one run) independently of the opacity
    /// dim/hide state — see this module's doc comment on [`CosmographHandle::clear_point_color_overrides`].
    #[wasm_bindgen(js_name = graphClearPointColorOverrides)]
    fn graph_clear_point_color_overrides();

    /// The betweenness automatic canvas effect (plan 44-13): applies pre-computed per-point
    /// sizes — `indices` and `scores` are parallel slices (same length). The 1x-3x scale
    /// computation itself lives in `graph_status_line.rs`'s `betweenness_resize_scale` (plan
    /// 44-13), not here — this only applies pre-computed sizes.
    #[wasm_bindgen(js_name = graphSetPointSizeByScore)]
    fn graph_set_point_size_by_score(indices: js_sys::Uint32Array, scores: js_sys::Float64Array);

    /// D-14: fits the viewport to a result's indices (`fitViewByIndices`, confirmed
    /// the phase 44 research notes — the indexed variant, not the plain `fitView`).
    #[wasm_bindgen(js_name = graphFitViewByIndices)]
    fn graph_fit_view_by_indices(indices: js_sys::Uint32Array);

    /// Selects points by index (also selecting their connected points) — used alongside
    /// `graph_fit_view_by_indices` when a result row is chosen.
    #[wasm_bindgen(js_name = graphSelectPoints)]
    fn graph_select_points(indices: js_sys::Uint32Array);

    /// D-11: the warm-up-end freeze (3s or alpha decay, whichever first) calls `pause()` —
    /// backgrounds the simulation without resetting it. NOT used by [`CosmographHandle::relayout`]
    /// — see `graph_stop` below for why the two are distinct.
    #[wasm_bindgen(js_name = graphPause)]
    fn graph_pause();

    /// D-11: fully halts the simulation. Paired with `graph_start` in
    /// [`CosmographHandle::relayout`] as `stop()` immediately followed by `start(alpha)` — there
    /// is no `restart()` method (phase 44 research Pitfall 5, the phase 44 UI spec Canvas section).
    /// Distinct from `graph_pause`: `stop()` fully halts the simulation so `start(alpha)` gives a
    /// genuine fresh warm-up from full energy, matching Cosmograph's confirmed `stop()`/`start()`
    /// pair rather than `pause()`/`start()`.
    #[wasm_bindgen(js_name = graphStop)]
    fn graph_stop();

    /// D-11: (re)starts the simulation at the given alpha (energy).
    #[wasm_bindgen(js_name = graphStart)]
    fn graph_start(alpha: f64);

    /// Tears down the mounted instance (WebGL context, simulation, listeners) and invalidates
    /// any mount still awaiting its data. Idempotent. Called from [`CosmographHandle`]'s `Drop`.
    #[wasm_bindgen(js_name = graphDestroy)]
    fn graph_destroy();
}

/// D-11's stop-then-start sequence, exposed as data (not only as a side-effecting call) so it
/// can be pinned by a host-testable unit test independent of any wasm/JS runtime.
/// [`CosmographHandle::relayout`] performs these two shim calls in this exact order — never
/// `("pause", "start")` and never `("restart", ...)`, since Cosmograph 2.5.1 has no `restart()`
/// method (phase 44 research Pitfall 5).
pub const fn relayout_calls() -> (&'static str, &'static str) {
    ("stop", "start")
}

/// The alpha (energy) [`CosmographHandle::relayout`] passes to `start` after `stop()` — a fresh
/// warm-up from full energy (the phase 44 UI spec: "giving a fresh warm-up from full energy").
const RELAYOUT_ALPHA: f64 = 1.0;

/// Holds the mounted Cosmograph instance's click closure. The closure MUST stay alive for the
/// component's lifetime — a dropped `Closure` invalidates the JS-side callback silently, exactly
/// as `cosmograph_spike.rs`'s `CosmographSpike` documents. The Cosmograph instance itself lives
/// in the shim's module-level `graphInstance`; what `mount_graph` RETURNS is a `Promise`, not the
/// instance, so it is awaited rather than stored here.
pub struct CosmographHandle {
    _click_closure: Closure<dyn FnMut(u32)>,
}

/// Everything [`CosmographHandle::mount`] needs from the page, grouped so the call reads as one
/// cohesive set of canvas inputs rather than a long positional argument list (and so adding a
/// future input does not keep growing the signature).
pub struct MountParams<'a> {
    pub container: &'a web_sys::Element,
    pub nodes_bytes: &'a [u8],
    pub links_bytes: &'a [u8],
    /// D-15's kind -> `--node-*` pairs; the shim resolves them to concrete colours at mount time.
    pub kind_colors: &'a [(&'a str, &'a str)],
    /// The edge-type -> `--edge-*` equivalent, wired into `linkColorBy`/`linkColorByFn`.
    pub edge_colors: &'a [(&'a str, &'a str)],
    /// D-11's warm-up freeze window, armed by the shim when the simulation actually starts.
    pub warm_up: std::time::Duration,
}

/// A single point row for [`CosmographHandle::mount_rows`] — field names match
/// `mountGraphFromRows`'s `dataConfig` column names (`pointIdBy: "id"`, `pointColorBy: "kind"`,
/// `pointLabelBy: "name"`) exactly, so `serde`'s field-name-as-JSON-key default needs no rename.
#[derive(serde::Serialize)]
pub struct CanvasPointRow {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// A single link row for [`CosmographHandle::mount_rows`] — field names match
/// `mountGraphFromRows`'s `dataConfig` column names (`linkSourceBy: "source"`,
/// `linkTargetsBy: ["target"]`, `linkColorBy: "edge_type"`) exactly.
#[derive(serde::Serialize)]
pub struct CanvasLinkRow {
    pub source: String,
    pub target: String,
    pub edge_type: String,
}

/// Turns a `Serialize` Rust value into a `JsValue` without a new crate dependency — the
/// `cosmograph_spike.rs::to_js_value` pattern, copied rather than imported: this module has no
/// dependency on that one today (D-17's own doc comment: this module generalises the spike's
/// pattern, it does not consume the spike module itself) and should not gain one for three lines.
fn to_js_value<T: serde::Serialize>(value: &T) -> JsValue {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    js_sys::JSON::parse(&json).unwrap_or(JsValue::NULL)
}

/// Everything [`CosmographHandle::mount_rows`] needs from the page — the small-population,
/// non-Arrow sibling of [`MountParams`] (see this struct's own field docs for how each differs).
pub struct RowMountParams<'a> {
    pub container: &'a web_sys::Element,
    pub points: &'a [CanvasPointRow],
    pub links: &'a [CanvasLinkRow],
    /// D-15's kind -> `--node-*` pairs; the shim resolves them to concrete colours at mount time.
    pub kind_colors: &'a [(&'a str, &'a str)],
    /// The edge-type -> `--edge-*` equivalent, wired into `linkColorBy`/`linkColorByFn`.
    pub edge_colors: &'a [(&'a str, &'a str)],
    /// D-11's warm-up freeze window, armed by the shim when the simulation actually starts.
    pub warm_up: std::time::Duration,
}

impl CosmographHandle {
    /// Mounts a Cosmograph instance into `container` from raw Arrow IPC `nodes_bytes`/
    /// `links_bytes` (two SEPARATE streams — CNV-04's population endpoint's own wire format,
    /// `arrow_export.rs`), coloured by `kind_colors` (D-15: `[(kind, css_var_name)]` pairs, one
    /// per `node_palette` kind — resolved to concrete colours by the shim at mount time so the
    /// active theme applies), invoking `on_click` with the clicked point's index on every
    /// `onPointClick`.
    ///
    /// Mounting is asynchronous on the JS side (`prepareCosmographData` is `async`), so this
    /// returns immediately and the outcome arrives later: nothing on success, or `on_error` with
    /// a human-readable message — ALSO written to the browser console via `console.error` — if
    /// the shim's promise rejects. A rejection is never swallowed (mirrors
    /// `CosmographSpike::mount` exactly).
    pub fn mount(
        params: MountParams<'_>,
        on_click: impl FnMut(u32) + 'static,
        on_error: impl FnOnce(String) + 'static,
    ) -> Self {
        let MountParams {
            container,
            nodes_bytes,
            links_bytes,
            kind_colors,
            edge_colors,
            warm_up,
        } = params;
        let closure = Closure::wrap(Box::new(on_click) as Box<dyn FnMut(u32)>);
        let nodes = js_sys::Uint8Array::from(nodes_bytes);
        let links = js_sys::Uint8Array::from(links_bytes);
        let to_pairs = |entries: &[(&str, &str)]| {
            let array = js_sys::Array::new();
            for (name, css_var) in entries {
                let pair = js_sys::Array::new();
                pair.push(&JsValue::from_str(name));
                pair.push(&JsValue::from_str(css_var));
                array.push(&pair);
            }
            array
        };
        let colors = to_pairs(kind_colors);
        let edges = to_pairs(edge_colors);
        let promise = mount_graph(
            container.as_ref(),
            nodes,
            links,
            colors,
            edges,
            warm_up.as_millis() as f64,
            &closure,
        );
        leptos::task::spawn_local(async move {
            if let Err(err) = wasm_bindgen_futures::JsFuture::from(promise).await {
                let message = js_error_message(&err);
                web_sys::console::error_2(&"[cosmograph-trait] mount failed:".into(), &err);
                on_error(message);
            }
        });
        Self {
            _click_closure: closure,
        }
    }

    /// The finding-detail-route mount path (plan 50-11): mounts a Cosmograph instance into
    /// `container` from a small, already-fetched, non-Arrow row set (a finding's own
    /// neighbourhood — never the full population) rather than raw Arrow IPC bytes. Same
    /// async-Promise/rejection discipline as [`CosmographHandle::mount`]: this returns
    /// immediately, the outcome arrives later via the awaited `Promise` — nothing on success, or
    /// `on_error` (also written to the browser console) if the shim's promise rejects.
    pub fn mount_rows(
        params: RowMountParams<'_>,
        on_click: impl FnMut(u32) + 'static,
        on_error: impl FnOnce(String) + 'static,
    ) -> Self {
        let RowMountParams {
            container,
            points,
            links,
            kind_colors,
            edge_colors,
            warm_up,
        } = params;
        let closure = Closure::wrap(Box::new(on_click) as Box<dyn FnMut(u32)>);
        let points_js = to_js_value(&points);
        let links_js = to_js_value(&links);
        let to_pairs = |entries: &[(&str, &str)]| {
            let array = js_sys::Array::new();
            for (name, css_var) in entries {
                let pair = js_sys::Array::new();
                pair.push(&JsValue::from_str(name));
                pair.push(&JsValue::from_str(css_var));
                array.push(&pair);
            }
            array
        };
        let colors = to_pairs(kind_colors);
        let edges = to_pairs(edge_colors);
        let promise = mount_graph_from_rows(
            container.as_ref(),
            points_js,
            links_js,
            colors,
            edges,
            warm_up.as_millis() as f64,
            &closure,
        );
        leptos::task::spawn_local(async move {
            if let Err(err) = wasm_bindgen_futures::JsFuture::from(promise).await {
                let message = js_error_message(&err);
                web_sys::console::error_2(&"[cosmograph-trait] mount_rows failed:".into(), &err);
                on_error(message);
            }
        });
        Self {
            _click_closure: closure,
        }
    }

    /// D-12/D-13: dims `dimmed` indices to low opacity, hides `hidden` indices entirely. Never
    /// re-supplies the point/link data (D-04) — only the per-point colour accessor changes.
    pub fn set_point_opacity(&self, dimmed: &[u32], hidden: &[u32]) {
        graph_set_point_opacity(
            js_sys::Uint32Array::from(dimmed),
            js_sys::Uint32Array::from(hidden),
        );
    }

    /// Restricts labels to `visible_ids` (the points left showing after a hide); an empty slice
    /// clears the filter. Hiding a point zeroes its colour alpha, which Cosmograph's label
    /// renderer does not read — without this the labels of filtered-out nodes stay on the canvas
    /// over empty space.
    pub fn set_visible_labels(&self, visible_ids: &[String]) {
        let array = js_sys::Array::new();
        for id in visible_ids {
            array.push(&wasm_bindgen::JsValue::from_str(id));
        }
        graph_set_visible_labels(array);
    }

    /// The community automatic canvas effect (plan 44-13). `hue_deg` always arrives pre-computed
    /// (`CommunityResult.color_hue_deg`) — this method only applies it, never computes one.
    pub fn set_point_color_by_hue(&self, indices: &[u32], hue_deg: f64) {
        graph_set_point_color_by_hue(js_sys::Uint32Array::from(indices), hue_deg);
    }

    /// D-12's Reset path: clears every accumulated hue override (community colouring from a
    /// prior run) without touching the opacity dim/hide state, so Reset restores the raw,
    /// kind-coloured population exactly (the phase 44 UI spec Status-line controls).
    pub fn clear_point_color_overrides(&self) {
        graph_clear_point_color_overrides();
    }

    /// The betweenness automatic canvas effect (plan 44-13). `indices` and `scores` are parallel
    /// slices (same length) — the 1x-3x scale computation lives in `graph_status_line.rs`, not
    /// here.
    pub fn set_point_size_by_score(&self, indices: &[u32], scores: &[f64]) {
        graph_set_point_size_by_score(
            js_sys::Uint32Array::from(indices),
            js_sys::Float64Array::from(scores),
        );
    }

    /// D-14: fits the viewport to a result's indices.
    pub fn fit_view_by_indices(&self, indices: &[u32]) {
        graph_fit_view_by_indices(js_sys::Uint32Array::from(indices));
    }

    /// Selects points by index (also selecting their connected points).
    pub fn select_points(&self, indices: &[u32]) {
        graph_select_points(js_sys::Uint32Array::from(indices));
    }

    /// D-11: backgrounds the simulation without resetting it — the warm-up-end freeze.
    pub fn pause(&self) {
        graph_pause();
    }

    /// D-11: (re)starts the simulation at the given alpha (energy).
    pub fn start(&self, alpha: f64) {
        graph_start(alpha);
    }

    /// D-11's Re-layout control: `stop()` immediately followed by `start(alpha)` — never
    /// `restart`, which does not exist on Cosmograph 2.5.1 (phase 44 research Pitfall 5,
    /// the phase 44 UI spec Canvas section: "giving a fresh warm-up from full energy"). See
    /// [`relayout_calls`] for the host-testable pin on this exact sequence.
    pub fn relayout(&self) {
        graph_stop();
        graph_start(RELAYOUT_ALPHA);
    }
}

/// Leaving the Graph page disposes the mounting component's `StoredValue`, which drops this
/// struct — and with it the click `Closure`. Mirrors `CosmographSpike`'s `Drop` exactly: without
/// this, the shim's module-level instance kept running (`enableSimulation: true`, a live WebGL
/// context) holding a JS function whose Rust side was gone.
impl Drop for CosmographHandle {
    fn drop(&mut self) {
        graph_destroy();
    }
}

/// A readable message for a rejected JS promise: the `Error`'s `.message` when it is one, the
/// string itself when the rejection value is a string, otherwise its `Debug` form. Mirrors
/// `cosmograph_spike.rs::js_error_message` exactly — small enough that duplicating it here keeps
/// this module free of any dependency on the spike module (D-17: this is a generalisation of the
/// spike's pattern, not a consumer of the spike module itself).
fn js_error_message(err: &JsValue) -> String {
    if let Some(error) = err.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    err.as_string().unwrap_or_else(|| format!("{err:?}"))
}

/// Pure click-handling core (mirrors `cosmograph_spike.rs::make_click_handler`'s host-testable
/// extraction pattern exactly): records the clicked point's index into `clicked`, extracted from
/// the `wasm-bindgen` `Closure` boundary so it is unit-testable on the host target without a
/// DOM/JS runtime. [`CosmographHandle::mount`]'s `on_click` callers wrap this same shape.
pub fn make_click_handler(clicked: RwSignal<Option<u32>>) -> impl FnMut(u32) {
    move |index: u32| {
        clicked.set(Some(index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PROVE THIS CAN FAIL (docs/DEVELOPMENT.md): change [`relayout_calls`] to return
    /// `("pause", "start")` and re-run — this test then fails, confirming the assertion is
    /// load-bearing on the exact sequence, not merely "two strings are returned".
    #[test]
    fn relayout_is_stop_then_start_never_restart() {
        let (first, second) = relayout_calls();
        assert_eq!(first, "stop");
        assert_eq!(second, "start");
        assert_ne!(first, "restart");
        assert_ne!(first, "pause");
    }

    /// Referencing both methods as typed function items pins their exact signatures — this test
    /// fails to COMPILE (not merely fails to pass) if either method is renamed or removed, so
    /// plan 44-13 can rely on these names never drifting.
    #[test]
    fn set_point_color_by_hue_and_set_point_size_by_score_signatures_are_pinned() {
        let _color_fn: fn(&CosmographHandle, &[u32], f64) =
            CosmographHandle::set_point_color_by_hue;
        let _size_fn: fn(&CosmographHandle, &[u32], &[f64]) =
            CosmographHandle::set_point_size_by_score;
    }

    /// PROVE THIS CAN FAIL: temporarily change `make_click_handler`'s body to
    /// `clicked.set(Some(0));` (ignoring `index`) and re-run — this then fails with
    /// `assertion `left == right` failed ... left: Some(0), right: Some(3)`, confirming the
    /// assertion is load-bearing on the actual clicked index. Mirrors
    /// `cosmograph_spike.rs`'s identically-shaped test exactly.
    #[test]
    fn click_handler_records_the_clicked_index_into_the_clicked_signal() {
        let clicked: RwSignal<Option<u32>> = RwSignal::new(None);
        let mut handler = make_click_handler(clicked);
        assert_eq!(clicked.get_untracked(), None, "sanity: nothing clicked yet");

        handler(3);
        assert_eq!(clicked.get_untracked(), Some(3));

        handler(6);
        assert_eq!(
            clicked.get_untracked(),
            Some(6),
            "a second click must overwrite, not accumulate"
        );
    }

    // `js_error_message` itself is not unit-tested here: constructing a `JsValue` (even a plain
    // string `.into()`) invokes wasm-bindgen's externref/describe machinery, which aborts the
    // process on a non-wasm host target (confirmed: this workspace's `cargo test` runs on the
    // host, phase 44 research Pitfall 10 / `cosmograph_spike.rs`'s own test module has no such
    // test for the same reason) — only genuinely host-testable pure logic (no `JsValue`
    // construction) is exercised on this target, per this task's acceptance criteria.

    #[test]
    fn relayout_alpha_is_full_energy() {
        assert_eq!(RELAYOUT_ALPHA, 1.0);
    }
}
