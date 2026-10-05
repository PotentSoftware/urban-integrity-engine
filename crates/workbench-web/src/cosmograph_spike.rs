//! Cosmograph interop spike (WBX-07, D-18) — mounts a small fixture graph via the vendored
//! `cosmograph-shim.js` bundle (`crates/workbench-web/js/`, a real directory in THIS crate: the
//! `module = "/js/..."` path below is read by the `wasm_bindgen` macro at expansion time relative
//! to this crate's `CARGO_MANIFEST_DIR` on every target, host builds included, so it must not go
//! through a symlink — see `js/README.md`) and wires a node-click callback back to Rust through
//! a stored `wasm-bindgen` closure. Reachable only at `/spike/cosmograph`, never linked from the
//! left nav (the phase 42 UI spec "Cosmograph spike route").
//!
//! The fixture graph below is hardcoded and contains no analysis-specific data — never any real
//! analysis population (D-10 amended: no analysis-specific strings anywhere, including spike
//! fixtures — enforced against this file by `scripts/check-no-analysis-literals.sh`).
//!
//! Module-wide `#![allow(dead_code)]`: Plan 43-09 removes the `/spike/cosmograph` route mount
//! from `app.rs` (D-07) while deliberately keeping this module and its `CosmographSpikePage`/
//! shim-binding code in the tree for Phase 44 to remount. With no call site left anywhere in the
//! crate, `cargo clippy -D warnings`'s dead-code lint would otherwise fire across this entire
//! file — the same interface-first situation `timestamp.rs`/`abbr.rs`/`entity_ref_view.rs`
//! documented from the opposite direction (not-yet-wired rather than unwired).
#![allow(dead_code)]

use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen(module = "/js/cosmograph-shim.js")]
extern "C" {
    /// `mountSpike` is an `async` JS function — the value it returns is a `Promise` that
    /// resolves once `prepareCosmographData` and the `Cosmograph` constructor have both
    /// completed, or REJECTS if either throws (no WebGL2, bad data config, ...). Typed as
    /// `js_sys::Promise` and awaited in [`CosmographSpike::mount`] precisely so that rejection
    /// reaches Rust; typed as a bare `JsValue` (the original shape) it was a silently-unhandled
    /// rejection and a blank `<div>` — the one failure mode a de-risking spike must not have.
    #[wasm_bindgen(js_name = mountSpike)]
    fn mount_spike(
        container: &JsValue,
        points: JsValue,
        links: JsValue,
        cb: &Closure<dyn FnMut(u32)>,
    ) -> js_sys::Promise;

    #[wasm_bindgen(js_name = spikeFitView)]
    #[allow(dead_code)]
    // exercised only by manual QA against a real browser, per the phase 42 validation notes
    fn spike_fit_view();

    #[wasm_bindgen(js_name = spikeSelectPoint)]
    #[allow(dead_code)]
    // exercised only by manual QA against a real browser, per the phase 42 validation notes
    fn spike_select_point(index: u32);

    /// Tears down the mounted instance (WebGL context, simulation, listeners) and invalidates
    /// any mount still awaiting its data. Idempotent. Called from [`CosmographSpike`]'s `Drop`.
    #[wasm_bindgen(js_name = spikeDestroy)]
    fn spike_destroy();
}

/// Holds the mounted Cosmograph spike's click closure. The closure MUST stay alive for the
/// component's lifetime — a dropped `Closure` invalidates the JS-side callback silently. This
/// struct is stored inside the mounting component's own reactive state (a
/// `StoredValue<_, LocalStorage>`, see `CosmographSpikePage` below), never a bare local variable
/// that would drop when the mounting function returns. The Cosmograph instance itself lives in
/// the shim's module-level `instance` (which `spikeFitView`/`spikeSelectPoint` operate on) —
/// what `mountSpike` RETURNS is a `Promise`, not the instance, so it is awaited rather than
/// stored.
pub struct CosmographSpike {
    _click_closure: Closure<dyn FnMut(u32)>,
}

impl CosmographSpike {
    /// Mounts a Cosmograph instance into `container` against the given fixture `points`/`links`
    /// (already converted to `JsValue`, see [`to_js_value`]), invoking `on_click` with the
    /// clicked point's index on every `onPointClick`.
    ///
    /// Mounting is asynchronous on the JS side (`prepareCosmographData` is `async`), so this
    /// returns immediately and the outcome arrives later: nothing on success, or `on_error`
    /// with a human-readable message — ALSO written to the browser console via
    /// `console.error` — if the shim's promise rejects. A rejection is never swallowed.
    pub fn mount(
        container: &web_sys::Element,
        points: JsValue,
        links: JsValue,
        on_click: impl FnMut(u32) + 'static,
        on_error: impl FnOnce(String) + 'static,
    ) -> Self {
        let closure = Closure::wrap(Box::new(on_click) as Box<dyn FnMut(u32)>);
        let promise = mount_spike(container.as_ref(), points, links, &closure);
        leptos::task::spawn_local(async move {
            if let Err(err) = wasm_bindgen_futures::JsFuture::from(promise).await {
                let message = js_error_message(&err);
                web_sys::console::error_2(&"[cosmograph-spike] mount failed:".into(), &err);
                on_error(message);
            }
        });
        Self {
            _click_closure: closure,
        }
    }
}

/// Leaving the `/spike/cosmograph` route disposes the component's `StoredValue`, which drops
/// this struct — and with it the click `Closure`. Without this `Drop`, the shim's module-level
/// instance kept running (`enableSimulation: true`, a live WebGL context) holding a JS function
/// whose Rust side was gone: wasm-bindgen throws "closure invoked recursively or after being
/// dropped" on the next click, and re-entering the route stacked a second instance on the
/// orphaned first. `spike_destroy` tears the instance down and invalidates any mount still in
/// flight, so the closure is only ever dropped AFTER nothing in JS can call it.
impl Drop for CosmographSpike {
    fn drop(&mut self) {
        spike_destroy();
    }
}

/// A readable message for a rejected JS promise: the `Error`'s `.message` when it is one, the
/// string itself when the rejection value is a string, otherwise its `Debug` form.
fn js_error_message(err: &JsValue) -> String {
    if let Some(error) = err.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    err.as_string().unwrap_or_else(|| format!("{err:?}"))
}

/// The exact copy the spike page shows when mounting fails (prefixed to the JS-side message) —
/// a visible, on-page failure, never a blank `<div>` with only a console entry behind it.
pub const MOUNT_ERROR_PREFIX: &str = "Cosmograph spike failed to mount: ";

/// The on-page error line for a mount failure: [`MOUNT_ERROR_PREFIX`] followed by the message.
pub fn mount_error_text(message: &str) -> String {
    format!("{MOUNT_ERROR_PREFIX}{message}")
}

/// A fixture graph point (matches the shim's `dataConfig.points.pointIdBy: 'id'`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FixturePoint {
    pub id: String,
    pub label: String,
}

/// A fixture graph link (matches the shim's `linkSourceBy`/`linkTargetsBy` data config).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FixtureLink {
    pub source: String,
    pub target: String,
}

/// 7 hardcoded fixture points (5-10 range per the plan's `<action>` text) — never any real
/// analysis data (D-10 amended: no analysis-specific strings anywhere, including spike
/// fixtures).
pub fn fixture_points() -> Vec<FixturePoint> {
    vec![
        FixturePoint {
            id: "n1".to_string(),
            label: "Alpha".to_string(),
        },
        FixturePoint {
            id: "n2".to_string(),
            label: "Bravo".to_string(),
        },
        FixturePoint {
            id: "n3".to_string(),
            label: "Charlie".to_string(),
        },
        FixturePoint {
            id: "n4".to_string(),
            label: "Delta".to_string(),
        },
        FixturePoint {
            id: "n5".to_string(),
            label: "Echo".to_string(),
        },
        FixturePoint {
            id: "n6".to_string(),
            label: "Foxtrot".to_string(),
        },
        FixturePoint {
            id: "n7".to_string(),
            label: "Golf".to_string(),
        },
    ]
}

/// 6 hardcoded fixture links joining the 7 fixture points into one small connected graph.
pub fn fixture_links() -> Vec<FixtureLink> {
    vec![
        FixtureLink {
            source: "n1".to_string(),
            target: "n2".to_string(),
        },
        FixtureLink {
            source: "n2".to_string(),
            target: "n3".to_string(),
        },
        FixtureLink {
            source: "n3".to_string(),
            target: "n4".to_string(),
        },
        FixtureLink {
            source: "n4".to_string(),
            target: "n5".to_string(),
        },
        FixtureLink {
            source: "n5".to_string(),
            target: "n6".to_string(),
        },
        FixtureLink {
            source: "n6".to_string(),
            target: "n7".to_string(),
        },
    ]
}

/// Converts any serializable fixture value into a `JsValue` via a JSON round-trip
/// (`serde_json` -> `js_sys::JSON::parse`). Avoids adding a `serde-wasm-bindgen` dependency for
/// this small, fixed-shape fixture (the phase 42 research notes' Cosmograph Spike section explicitly leaves
/// this an open implementation choice — this crate already depends on `serde_json`/`js-sys`, so
/// the JSON round-trip needs no new dependency).
fn to_js_value<T: serde::Serialize>(value: &T) -> JsValue {
    let json = serde_json::to_string(value).expect("fixture serialization should never fail");
    js_sys::JSON::parse(&json).expect("fixture JSON should always parse")
}

/// Pure click-handling core: records the clicked point's index into the `clicked` signal — the
/// SAME signal [`CosmographSpikePage`] reads, so the unit test below exercises the path the
/// component actually relies on (the original version wrote into a private `Rc<RefCell<_>>`
/// nothing ever read, while the component set `clicked` separately; the test then proved a
/// function the page did not depend on — Phase 42 code review, IN-02). Extracted from the
/// `wasm-bindgen` `Closure` boundary so it is unit-testable on the host target without a DOM/JS
/// runtime — the `Closure`/`mount_spike` glue itself only does anything observable inside a real
/// browser, which this workspace's CI cannot exercise headlessly (phase 42 research Pitfall 5).
/// The console log lives in the component, not here: `web_sys::console` is a wasm-bindgen import
/// and panics when called on a non-wasm target.
///
/// PROVE THIS CAN FAIL (docs/DEVELOPMENT.md): the test below asserts the exact clicked index is
/// recorded, not merely that the signal changed — a handler that always writes a fixed index
/// (e.g. `Some(0)`) regardless of the clicked point would compile and pass a weaker assertion but
/// fail this one.
pub fn make_click_handler(clicked: RwSignal<Option<u32>>) -> impl FnMut(u32) {
    move |index: u32| {
        clicked.set(Some(index));
    }
}

/// The `/spike/cosmograph` route (the phase 42 UI spec "Cosmograph spike route"): a bare full-height
/// `<div>`, no chrome, mounting the Cosmograph spike against the hardcoded fixture graph on
/// first render. The click index is both logged to the browser console (observable for manual
/// QA, the phase 42 validation notes' Manual-Only Verifications row) and reflected in the `clicked` signal
/// so a future automated UI test could assert on it without parsing console output. A mount
/// failure (the shim's promise rejecting) is rendered on the page via `mount_error`, not left as
/// a blank canvas.
#[component]
pub fn CosmographSpikePage() -> impl IntoView {
    let clicked: RwSignal<Option<u32>> = RwSignal::new(None);
    let mount_error: RwSignal<Option<String>> = RwSignal::new(None);
    let spike: StoredValue<Option<CosmographSpike>, LocalStorage> = StoredValue::new_local(None);
    let container: NodeRef<leptos::html::Div> = NodeRef::new();

    Effect::new(move |_| {
        let Some(el) = container.get() else {
            return;
        };
        // Mount exactly once — a NodeRef-triggered Effect can re-run on unrelated reactive
        // updates; re-mounting would leak a second Cosmograph instance and closure.
        let already_mounted = spike.with_value(|s| s.is_some());
        if already_mounted {
            return;
        }

        // The tested core records into `clicked`; the console line is the manual-QA hook.
        let mut record_click = make_click_handler(clicked);
        let logged_click = move |index: u32| {
            record_click(index);
            web_sys::console::log_1(
                &format!("[cosmograph-spike] node clicked: index={index}").into(),
            );
        };

        let points = to_js_value(&fixture_points());
        let links = to_js_value(&fixture_links());
        let mounted = CosmographSpike::mount(&el, points, links, logged_click, move |message| {
            mount_error.set(Some(message));
        });
        spike.set_value(Some(mounted));
    });

    view! {
        <div style="position: relative;">
            {move || mount_error.get().map(|message| view! {
                <p
                    class="text-body"
                    role="alert"
                    style="position: absolute; top: var(--space-md); left: var(--space-md); right: var(--space-md); margin: 0; padding: var(--space-sm) var(--space-md); background: var(--card); border: 1px solid var(--border); border-radius: 8px; color: var(--foreground); z-index: 1;"
                >
                    {mount_error_text(&message)}
                </p>
            })}
            <div
                node_ref=container
                style="height: calc(100vh - 56px); background: var(--background);"
            ></div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PROVE THIS CAN FAIL (docs/DEVELOPMENT.md): temporarily change `make_click_handler`'s body
    /// to `clicked.set(Some(0));` (ignoring `index`) and re-run — this test then fails with
    /// `assertion `left == right` failed ... left: Some(0), right: Some(3)`, confirming the
    /// assertion is load-bearing on the actual clicked index, not merely "some value was set".
    ///
    /// Reads the SAME `RwSignal` shape the component hands to `make_click_handler`, so this is
    /// the page's real click path, not a parallel sink. `RwSignal::new` works on the host target
    /// without a reactive owner (the arena slot is simply never reclaimed — fine for a test).
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

    /// A mount failure is rendered as visible page copy carrying the JS-side message — the
    /// prefix is fixed so manual QA (the phase 42 validation notes) can grep for it.
    #[test]
    fn mount_error_text_carries_the_prefix_and_the_message() {
        let text = mount_error_text("WebGL2 is not available");
        assert!(text.starts_with(MOUNT_ERROR_PREFIX));
        assert!(text.ends_with("WebGL2 is not available"));
    }

    #[test]
    fn fixture_points_are_in_the_five_to_ten_range_with_unique_ids() {
        let points = fixture_points();
        assert!(
            (5..=10).contains(&points.len()),
            "fixture should have 5-10 points per the plan's <action> text, got {}",
            points.len()
        );
        let unique_ids: std::collections::HashSet<_> = points.iter().map(|p| &p.id).collect();
        assert_eq!(
            unique_ids.len(),
            points.len(),
            "fixture point ids must be unique"
        );
    }

    #[test]
    fn fixture_links_reference_only_known_fixture_point_ids() {
        let point_ids: std::collections::HashSet<_> =
            fixture_points().into_iter().map(|p| p.id).collect();
        let links = fixture_links();
        assert!(!links.is_empty(), "fixture should have at least one link");
        for link in &links {
            assert!(
                point_ids.contains(&link.source),
                "link source {} must reference a known fixture point",
                link.source
            );
            assert!(
                point_ids.contains(&link.target),
                "link target {} must reference a known fixture point",
                link.target
            );
        }
    }
}

// Note: no in-crate "fixture data contains no analysis-specific strings" test lives here — a
// test asserting a hardcoded, forbidden fixture literal is absent would itself have to spell
// out that same literal, which is exactly what `scripts/check-no-analysis-literals.sh`'s
// CI-level, case-insensitive file scan (D-10 amended) flags. That script is the authoritative,
// single-source guard for this file and every other workbench source file; duplicating it
// in-crate would only reintroduce the forbidden literal into the source it exists to keep clean.
