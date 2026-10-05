// cosmograph-shim-src.js — source for the vendored, pinned Cosmograph ESM bundle (D-16, WBX-07).
//
// This file is bundled once via `npm install && npx esbuild cosmograph-shim-src.js --bundle
// --format=esm --outfile=../js/cosmograph-shim.js` — the OUTPUT is what's committed
// and consumed by `crates/workbench-web/src/cosmograph_spike.rs`'s
// `#[wasm_bindgen(module = "/js/cosmograph-shim.js")]` binding. This source file is committed
// alongside `package.json` for reproducibility, but is never itself consumed at Cargo build time.
//
// BECAUSE the bundle is what ships and this file is not, the two can drift without anything
// failing — and did, for eight days: `pointLabelBy`, `graphEdgeColorMap`/`linkColorByFn`,
// `showHoveredPointLabel` and `armWarmUpFreeze` were edited straight into the bundle by 278a4a38
// and c2f344eb and never written back, so the documented `npm run build` would have silently
// reverted all four. Reconciled 2026-09-13. `scripts/check-shim-drift.sh` now rebuilds this file
// and compares it against the vendored bundle — run it after ANY shim change, and before ever
// regenerating the bundle.
//
// Exact callback/method names verified 2026-09-04 against official cosmograph.app docs
// (the phase 42 research notes "Cosmograph Spike" section) — `onPointClick`, `fitView`, `selectPoint`,
// `prepareCosmographData` are the real, confirmed API surface (not the milestone research's
// approximate `selectNodes`). `destroy()` (returns `Promise<void>`) confirmed against the
// package's own `cosmograph/index.d.ts` at the pinned 2.5.1.
import { Cosmograph, prepareCosmographData } from '@cosmograph/cosmograph';

let instance = null;

// Bumped by every `spikeDestroy()`. `mountSpike` awaits `prepareCosmographData` before it can
// construct anything; if the Rust side drops (route change) during that await, the generation
// check below stops the late-arriving instance from resurrecting itself against a closure that
// no longer exists ("closure invoked recursively or after being dropped" on the first click).
let generation = 0;

/**
 * Mounts a Cosmograph instance against a small fixture graph and wires `onNodeClick` to fire on
 * every point click. Returns the mounted instance (also kept in the module-local `instance` for
 * `spikeFitView`/`spikeSelectPoint`/`spikeDestroy` to operate on), or `null` if the spike was
 * destroyed while the data was still being prepared.
 *
 * This is an `async` function: the caller receives a Promise. A failure anywhere in here
 * (`prepareCosmographData` rejecting, the `Cosmograph` constructor throwing — e.g. no WebGL2)
 * is logged to the console AND rethrown so the Rust side, which awaits this Promise, sees it.
 *
 * @param {Element} container - the DOM element to mount into
 * @param {Array<object>} points - fixture point records, each carrying an `id` field
 * @param {Array<object>} links - fixture link records, each carrying `source`/`target` fields
 * @param {(index: number) => void} onNodeClick - invoked with the clicked point's index
 */
export async function mountSpike(container, points, links, onNodeClick) {
  // Never two live instances: a stale one (defensive — the Rust side mounts once per component
  // lifetime and destroys on drop) is torn down before the new one is prepared.
  spikeDestroy();
  const myGeneration = ++generation;
  try {
    const dataConfig = {
      points: { pointIdBy: 'id' },
      links: { linkSourceBy: 'source', linkTargetsBy: ['target'] },
    };
    const { points: p, links: l, cosmographConfig } = await prepareCosmographData(
      dataConfig,
      points,
      links,
    );
    const created = new Cosmograph(container, {
      ...cosmographConfig,
      points: p,
      links: l,
      enableSimulation: true,
      onPointClick: (index, event) => onNodeClick(index),
    });
    if (myGeneration !== generation) {
      // Destroyed while preparing — do not keep an instance whose click callback is gone.
      await created.destroy();
      return null;
    }
    instance = created;
    return created;
  } catch (err) {
    console.error('[cosmograph-spike] mountSpike failed:', err);
    throw err;
  }
}

/** Fits the camera to the currently mounted graph (WBX-07 spike proof surface). */
export function spikeFitView() {
  instance?.fitView(500, 0.1);
}

/** Selects a single point by index, also selecting its connected points (spike proof surface). */
export function spikeSelectPoint(index) {
  instance?.selectPoint(index, false, true);
}

/**
 * Tears down the mounted instance — WebGL context, simulation, event listeners — and
 * invalidates any `mountSpike` still awaiting its data. Idempotent; safe to call with nothing
 * mounted. Called from the Rust side's `Drop`, so a rejection from `destroy()` is logged here
 * rather than thrown back across the wasm boundary.
 */
export function spikeDestroy() {
  generation++;
  const current = instance;
  instance = null;
  if (current) {
    Promise.resolve(current.destroy()).catch((err) =>
      console.error('[cosmograph-spike] destroy failed:', err),
    );
  }
}

// ─── Graph page (Phase 44, D-17) ────────────────────────────────────────────
//
// Everything below is the Graph page's mount/opacity/colour/size/fit/select/pause/stop/start/
// destroy surface — consumed exclusively through `crates/workbench-web/src/cosmograph_trait.rs`'s
// `CosmographHandle` (D-17: no page or algorithm code calls these exports directly). Kept in a
// SEPARATE module-level `graphInstance`/`graphGeneration` pair from the spike's `instance`/
// `generation` above — the Graph page (`/graph`) and the spike route (`/spike/cosmograph`) are
// never mounted simultaneously, but keeping the two state machines independent avoids any
// coupling between the Phase 42 spike record and the Phase 44 page it seeds.

let graphInstance = null;
let graphGeneration = 0;

// D-15: the resolved kind -> CONCRETE colour string map built at mount time from `mountGraph`'s
// `kindColorEntries` argument (`[[kind, cssVarName], ...]`) — each `--node-*` custom-property
// name resolved via `getComputedStyle(document.documentElement)` so the theme active AT MOUNT
// (light or dark) applies, never the raw property name. Consumed by `graphBaseColor` below as
// the fallback colour for the "no dim/hide/hue override active" case (`pointColorByFn`'s own
// base case) — the primary kind colouring itself is driven by `pointColorBy: 'kind'` +
// `pointColorByMap` set directly in `mountGraph`'s `dataConfig.points`. Cleared in `graphDestroy`
// so a stale map never survives into a fresh mount.
let graphKindColorMap = new Map();

// D-15's edge-type counterpart to `graphKindColorMap`: the resolved edge type -> CONCRETE colour
// string map built at mount time from `mountGraph`'s `edgeColorEntries` argument
// (`[[edgeType, cssVarName], ...]`), each `--edge-*` custom property resolved through
// `resolveCssVar` so the theme active AT MOUNT applies. Cosmograph exposes no `linkColorByMap`
// (only points get one), so this map is consumed by the `linkColorByFn` `mountGraph` installs.
// Cleared in `graphDestroy` so a stale (possibly wrong-theme) map never survives a fresh mount.
let graphEdgeColorMap = new Map();

// The pending `setTimeout` handle for D-11's warm-up poll (`armWarmUpFreeze`); `null` whenever no
// tick is scheduled. Cleared in `graphDestroy` so a torn-down mount's poll cannot outlive it.
let graphWarmUpTimer = null;

// D-11's warm-up freeze, driven by OBSERVED simulation state rather than a lifecycle callback.
//
// Two earlier approaches failed. A `setTimeout` from mount fired while the simulation had not
// started yet (duckdb-wasm init plus the data upload outlast the warm-up window on a cold load),
// and Cosmograph's `pause()` is a silent no-op unless `isSimulationRunning` is already true — so
// nothing ever froze. Hooking `onSimulationStart` did not help either: that callback is never
// delivered to a config passed through this shim, verified by instrumenting it and seeing no call
// at all while the graph was visibly moving.
//
// What IS verified to work is `pause()` itself, whenever the simulation is genuinely running. So
// this polls for that state: it starts the warm-up clock when the simulation is first seen
// running, freezes once the window has elapsed, and keeps watching afterwards so a later restart
// (Cosmograph's own post-upload restart, or a deliberate Re-layout) settles the same way instead
// of running forever.
function armWarmUpFreeze(myGeneration, warmUpMs) {
  const POLL_MS = 250;
  let runningSince = null;
  const tick = () => {
    graphWarmUpTimer = null;
    if (myGeneration !== graphGeneration) return; // a newer mount superseded this one
    const running = graphInstance?._cosmos?.store?.isSimulationRunning === true;
    if (!running) {
      runningSince = null;
    } else {
      if (runningSince === null) runningSince = Date.now();
      if (Date.now() - runningSince >= warmUpMs) {
        graphInstance?.pause();
        runningSince = null;
      }
    }
    graphWarmUpTimer = setTimeout(tick, POLL_MS);
  };
  graphWarmUpTimer = setTimeout(tick, POLL_MS);
}

// Fallback flat grey used ONLY when a point's `kind` is not in `graphKindColorMap` (an
// unrecognised kind never emitted by `arrow_export.rs::node_kind_name`'s exhaustive match, or no
// instance mounted yet) — mirrors `node_palette.rs::UNKNOWN_KIND_COLOR`'s "uncategorised, not a
// new undocumented colour" rationale, kept as a last-resort default rather than every unmapped
// kind silently becoming invisible.
const GRAPH_BASE_COLOR_RGB = [148, 148, 148];
const GRAPH_BASE_POINT_SIZE = 4;
// Links keep Cosmograph's own resting colour; the override layer only edits alpha — dimmed
// (a neighbour of a dimmed point) or 0 (either endpoint hidden).
const GRAPH_LINK_DIMMED_ALPHA = 0.06;
// Per-link endpoint POINT indices from the data-kit's own `sourceidx`/`targetidx` columns on the
// prepared links table, captured at mount as the fallback when cosmos's `graph.links` pairs are
// not readable; `null` until mounted and after destroy.
let graphLinkEnds = null;

/** Reads the data-kit's `sourceidx`/`targetidx` columns off the prepared links Arrow table. */
function captureLinkEnds(linksTable) {
  try {
    const source = linksTable?.getChild?.('sourceidx')?.toArray?.();
    const target = linksTable?.getChild?.('targetidx')?.toArray?.();
    if (source && target && source.length === target.length) {
      return { source, target };
    }
    console.warn('[cosmograph-graph] links table carries no sourceidx/targetidx columns — edges will not follow point hide/dim');
  } catch (err) {
    console.warn('[cosmograph-graph] could not read link endpoint columns:', err);
  }
  return null;
}
const GRAPH_DIMMED_ALPHA = 0.12;
const GRAPH_HUE_SATURATION = 0.65;
const GRAPH_HUE_LIGHTNESS = 0.55;

// Per-index hue overrides accumulated by `graphSetPointColorByHue` across multiple calls within
// one algorithm run (one call per community group) — NOT cleared per call, since a later call
// for a different community must not erase an earlier community's colouring within the same run.
// Cleared only by `graphClearPointColorOverrides` (D-12's Reset path) or `graphDestroy`.
let pointHueOverrides = new Map();

/** Resolves a `--css-var-name` custom property to its CURRENT concrete colour string via the
 * live computed style of `:root` — so the theme active at mount time (light or dark) is what
 * gets baked into `graphKindColorMap`, never the raw variable name Cosmograph cannot interpret. */
function resolveCssVar(cssVarName) {
  return getComputedStyle(document.documentElement).getPropertyValue(cssVarName).trim();
}

/**
 * Converts an HSL colour (hue in degrees; saturation/lightness in [0,1]) to the RGBA tuple
 * `[r, g, b, a]` (0-255 integer channels, alpha in [0,1]) `pointColorByFn` returns, per the live
 * `@cosmograph/cosmograph@2.5.1` TypeScript types.
 */
function hslToRgba(hueDeg, saturation, lightness, alpha) {
  const h = (((hueDeg % 360) + 360) % 360) / 360;
  if (saturation === 0) {
    const v = Math.round(lightness * 255);
    return [v, v, v, alpha];
  }
  const q =
    lightness < 0.5
      ? lightness * (1 + saturation)
      : lightness + saturation - lightness * saturation;
  const p = 2 * lightness - q;
  const hue2rgb = (t) => {
    let tt = t;
    if (tt < 0) tt += 1;
    if (tt > 1) tt -= 1;
    if (tt < 1 / 6) return p + (q - p) * 6 * tt;
    if (tt < 1 / 2) return q;
    if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
    return p;
  };
  const r = Math.round(hue2rgb(h + 1 / 3) * 255);
  const g = Math.round(hue2rgb(h) * 255);
  const b = Math.round(hue2rgb(h - 1 / 3) * 255);
  return [r, g, b, alpha];
}

/** The colour a point falls back to when no dim/hide override and no hue override apply — D-15's
 * per-`kind` colour resolved into `graphKindColorMap` at mount time, or the flat grey
 * `GRAPH_BASE_COLOR_RGB` if `kind` is not in the map (never mounted yet, or an unrecognised
 * kind). `kind` is the exact value `pointColorByFn`'s accessor receives as its first argument —
 * the `pointColorBy: 'kind'` column's raw value for that point/row, per the live
 * `@cosmograph/cosmograph@2.5.1` `AccessorFn<T, O> = (value: T, index?: number) => O` contract. */
function graphBaseColor(kind) {
  return graphKindColorMap.get(kind) ?? [...GRAPH_BASE_COLOR_RGB, 1.0];
}

// Resolves the D-15 kind/edge-type -> CSS-var colour pairs into the module-level
// `graphKindColorMap`/`graphEdgeColorMap` (read later by `linkColorByFn` and by the
// point-recolour effects below). Shared by `mountGraph` and `mountGraphFromRows` so the two
// mount paths can never disagree about how a kind/edge-type resolves to a concrete colour.
function resolveGraphColorMaps(kindColorEntries, edgeColorEntries) {
  graphKindColorMap = new Map(
    kindColorEntries.map(([kind, cssVarName]) => [kind, resolveCssVar(cssVarName)]),
  );
  // Edge-type -> concrete colour, resolved from the --edge-* CSS custom properties at mount
  // time exactly as the kind colours are. Cosmograph has no linkColorByMap (only points get
  // one), so the mapping is applied through linkColorByFn: naming BOTH linkColorBy (which
  // selects the edge_type column) and linkColorByFn (which switches the library to its
  // "Direct" link-colour strategy) makes this lookup authoritative, instead of letting
  // Cosmograph build its own ordinal scale over whatever values happen to be present — which
  // would repaint edges differently per population and silently desynchronise the legend.
  graphEdgeColorMap = new Map(
    (edgeColorEntries || []).map(([edgeType, cssVarName]) => [edgeType, resolveCssVar(cssVarName)]),
  );
}

// The `dataConfig` shape both mount paths hand to `prepareCosmographData` -- identical column
// names (`id`/`name`/`kind` for points, `source`/`target`/`edge_type` for links) whether the
// underlying data arrives as Arrow IPC bytes (`mountGraph`) or plain JSON row objects
// (`mountGraphFromRows`), so this one definition serves both.
function graphDataConfig() {
  return {
    points: {
      pointIdBy: 'id',
      pointColorBy: 'kind',
      pointColorByMap: Object.fromEntries(graphKindColorMap),
      // The node label: without pointLabelBy there is no label column, so Cosmograph renders
      // no text at all no matter what the show*Labels flags say (they all default to true).
      pointLabelBy: 'name',
    },
    links: {
      linkSourceBy: 'source',
      linkTargetsBy: ['target'],
      linkColorBy: 'edge_type',
    },
  };
}

// The shared tail both `mountGraph` (Arrow IPC bytes) and `mountGraphFromRows` (plain JSON-row
// arrays, the finding-detail-route mount path) call once their own point/link data is ready:
// `prepareCosmographData`, constructing the `Cosmograph` instance with the exact same
// link/label/click wiring, the `myGeneration` staleness check, and arming the D-11 warm-up
// freeze. Neither caller duplicates any of this.
async function finishGraphMount(
  myGeneration,
  container,
  dataConfig,
  pointsData,
  linksData,
  warmUpMs,
  onNodeClick,
) {
  const edgeFallbackColor = resolveCssVar('--muted-foreground');
  const prepared = await prepareCosmographData(dataConfig, pointsData, linksData);
  // `prepareCosmographData` swallows its own failures and resolves `undefined` — observed
  // 2026-09-08 for a 0-row points table (an empty population export), which otherwise
  // surfaces only as "Cannot destructure property 'points' of undefined". Name the cause; the
  // data-kit's own reason is on the console as a `cosmographLog(..., "data-kit", "error")`.
  if (!prepared || !prepared.points) {
    throw new Error(
      'Cosmograph could not prepare the population — the export is empty or unreadable; see ' +
        'the data-kit error logged above',
    );
  }
  const { points: p, links: l, cosmographConfig } = prepared;
  graphLinkEnds = captureLinkEnds(l);
  graphContainer = container;
  graphIdToName = captureIdToName(p);
  const created = new Cosmograph(container, {
    ...cosmographConfig,
    points: p,
    links: l,
    enableSimulation: true,
    // Both of these must be repeated on the INSTANCE config, not only inside the dataConfig
    // handed to prepareCosmographData: Cosmograph's own label and link-colour code paths read
    // `config.pointLabelBy` / `config.linkColorBy` off the live instance (see its hovered-label
    // guard, which bails when `pointLabelBy` is falsy). Setting them only on the data config
    // prepares the columns and then renders neither.
    // Link legibility (operator QA 2026-09-10: "links need to be more visible and brighter").
    // Cosmos fades links by their SCREEN-SPACE length: anything shorter than
    // `linkVisibilityDistanceRange[0]` is drawn at `linkVisibilityMinTransparency`, whose
    // default 0.25 renders most of a settled layout's links at a quarter alpha regardless of
    // the `--edge-*` colour resolved for them. Raising the floor is what actually makes the
    // edge-type palette readable; `linkWidthScale` then gives the 1px default line enough
    // body to show its colour at all.
    linkWidthScale: 2,
    linkVisibilityMinTransparency: 0.9,
    pointLabelBy: 'name',
    linkColorBy: 'edge_type',
    linkColorByFn: (edgeType) => graphEdgeColorMap.get(edgeType) ?? edgeFallbackColor,
    // Every other label flag already defaults to true (showLabels/showTopLabels/
    // showDynamicLabels); only the hovered-point label is off by default, and it is the one
    // that names a node at any zoom level, including when the dynamic-label limit hides the
    // rest.
    showHoveredPointLabel: true,
    // D-11's warm-up freeze is armed from the moment the simulation ACTUALLY starts, never
    // from mount. Cosmograph's own pause() is a no-op unless `isSimulationRunning` is already
    // true, and the simulation only begins after duckdb-wasm initialises and the data uploads
    // — comfortably longer than the warm-up window on a cold load. A timer started at mount
    // therefore fired into a not-yet-running simulation, did nothing, and left the layout
    // running forever: the graph never settled, kept expanding past the viewport, and points
    // drifted under the cursor.
    onPointClick: (index, event) => onNodeClick(index),
  });
  if (myGeneration !== graphGeneration) {
    // Destroyed while preparing — do not keep an instance whose click callback is gone.
    await created.destroy();
    return null;
  }
  graphInstance = created;
  armWarmUpFreeze(myGeneration, warmUpMs);
  return created;
}

/**
 * Mounts the Graph page's Cosmograph instance from TWO SEPARATE raw Arrow IPC byte streams — one
 * for points, one for links (D-04, CNV-04): `prepareCosmographData(config, pointsData,
 * linksData?)` takes points and links as SEPARATE arguments (confirmed against the live
 * `@cosmograph/cosmograph@2.5.1` typings, the phase 44 research notes), matching
 * `crates/workbench/src/graph/arrow_export.rs::build_arrow_ipc`'s own two-stream wire format —
 * no JS-side Arrow parsing either way. Colours every point by its `kind` column (D-15):
 * `kindColorEntries` (`[[kind, cssVarName], ...]`) is resolved to CONCRETE colour strings via
 * `resolveCssVar` at mount time (so the active theme applies) and wired into
 * `pointColorBy: 'kind'` + `pointColorByMap` in `dataConfig.points` — Cosmograph's own strategy
 * resolver (`resolveOptimalPointColorStrategy`) then picks the `'map'` strategy automatically
 * whenever `pointColorByFn` is NOT also set, exactly the mount-time case here. `pointColorStrategy`
 * is deliberately left UNSET (never forced to `'map'`): forcing it would short-circuit that same
 * resolver and permanently ignore the `pointColorByFn` the opacity/hue override paths
 * (`graphSetPointOpacity`/`graphSetPointColorByHue`) set later via `setConfig` — those rely on the
 * resolver falling through to the `'direct'` strategy the moment a `pointColorByFn` is present.
 * Data is supplied exactly once here; there is no `setData()` method on Cosmograph 2.5.1, matching
 * D-04's "no re-marshalling of the link set on a toggle" exactly. Same generation-guard /
 * rejection / console-logging discipline as `mountSpike` above.
 *
 * @param {Element} container - the DOM element to mount into
 * @param {Uint8Array} nodesBytes - the nodes stream's raw Arrow IPC bytes (`id`, `kind`, `name`
 *   columns, per `arrow_export.rs`)
 * @param {Uint8Array} linksBytes - the links stream's raw Arrow IPC bytes (`source`, `target`,
 *   `edge_type` columns, per `arrow_export.rs`)
 * @param {Array<[string, string]>} kindColorEntries - flat `[kind, cssVarName]` pairs, one per
 *   `node_palette.rs` kind (`NODE_KIND_PALETTE`/`node_kind_color`'s full domain) — resolved to
 *   concrete colours here via `resolveCssVar`; Cosmograph itself cannot resolve CSS custom
 *   properties, only literal colour strings/RGBA arrays.
 * @param {Array<[string, string]>} edgeColorEntries - flat `[edgeType, cssVarName]` pairs, one per
 *   `edge_palette.rs` edge type, resolved here the same way and applied through `linkColorByFn`
 *   (Cosmograph has no `linkColorByMap`).
 * @param {number} warmUpMs - D-11's warm-up window in milliseconds, handed to `armWarmUpFreeze`
 *   and measured from the moment the simulation is first OBSERVED running, never from mount.
 * @param {(index: number) => void} onNodeClick - invoked with the clicked point's index
 */
export async function mountGraph(
  container,
  nodesBytes,
  linksBytes,
  kindColorEntries,
  edgeColorEntries,
  warmUpMs,
  onNodeClick,
) {
  // Never two live instances (defensive — the Rust side mounts once per component lifetime and
  // destroys on drop, same discipline as `mountSpike`).
  graphDestroy();
  const myGeneration = ++graphGeneration;
  try {
    resolveGraphColorMaps(kindColorEntries, edgeColorEntries);
    const dataConfig = graphDataConfig();
    return await finishGraphMount(myGeneration, container, dataConfig, nodesBytes, linksBytes, warmUpMs, onNodeClick);
  } catch (err) {
    console.error('[cosmograph-graph] mountGraph failed:', err);
    throw err;
  }
}

// The finding-detail-route mount path (plan 50-11): the same ingestion shape `mountSpike`
// already proved `prepareCosmographData` accepts (plain JS arrays of point/link objects, no
// Arrow framing), with `mountGraph`'s full kind/edge colouring and click wiring on top via
// `finishGraphMount`. `pointsRows`/`linksRows` are plain arrays of `{ id, name, kind }` /
// `{ source, target, edge_type }` objects -- the Rust side (`CanvasPointRow`/`CanvasLinkRow`)
// emits exactly these field names.
export async function mountGraphFromRows(
  container,
  pointsRows,
  linksRows,
  kindColorEntries,
  edgeColorEntries,
  warmUpMs,
  onNodeClick,
) {
  graphDestroy();
  const myGeneration = ++graphGeneration;
  try {
    resolveGraphColorMaps(kindColorEntries, edgeColorEntries);
    const dataConfig = graphDataConfig();
    return await finishGraphMount(myGeneration, container, dataConfig, pointsRows, linksRows, warmUpMs, onNodeClick);
  } catch (err) {
    console.error('[cosmograph-graph] mountGraphFromRows failed:', err);
    throw err;
  }
}

// ─── Override layer: drives the cosmos renderer directly ─────────────────────────────────────
//
// `pointColorByFn`/`pointSizeByFn`/`linkColorByFn` are DATA config keys in Cosmograph 2.5.1
// (`config/interfaces/data.d.ts`): every `setConfig` naming one re-runs
// `_uploadAndValidateGraphData`, which — with no `points` in the delta — resets the graph and logs
// "Points source data is not provided in `points`." (observed 2026-09-08 on the first community
// row click; Reset died the same way). Re-supplying `points` would re-upload 113k rows to DuckDB
// per click and contradict D-04. So overrides bypass config entirely and write per-point /
// per-link RGBA and size arrays straight into the underlying cosmos `Graph` — exactly the seam
// Cosmograph's own `data-manager.js` uses (`this._.cosmos.setPointColors(...)`). Base arrays are
// captured once from the renderer on the first override (after Cosmograph applied the D-15 kind
// colouring) and restored verbatim by `graphClearPointColorOverrides`.
let graphBasePointColors = null; // Float32Array, RGBA per point, 0..1
let graphBasePointSizes = null; // Float32Array, one per point
let graphBaseLinkColors = null; // Float32Array, RGBA per link, 0..1

/** The underlying cosmos `Graph` (pinned 2.5.1 internals: `_cosmos`, else `_.cosmos`). */
function graphCosmos() {
  return graphInstance?._cosmos ?? graphInstance?._?.cosmos ?? null;
}

/** Captures the renderer's current colour/size arrays as the "no overrides" baseline, once. */
function ensureGraphBaseline() {
  const cosmos = graphCosmos();
  if (!cosmos) return null;
  if (!graphBasePointColors) {
    const c = cosmos.getPointColors?.();
    if (c && c.length) graphBasePointColors = Float32Array.from(c);
  }
  if (!graphBasePointSizes) {
    const s = cosmos.getPointSizes?.();
    if (s && s.length) graphBasePointSizes = Float32Array.from(s);
  }
  if (!graphBaseLinkColors) {
    const l = cosmos.getLinkColors?.();
    if (l && l.length) graphBaseLinkColors = Float32Array.from(l);
  }
  return graphBasePointColors ? cosmos : null;
}

/** Per-link endpoint point indices: cosmos's own `graph.links` pairs, else the data-kit columns. */
function graphLinkEndpointArrays(cosmos, linkCount) {
  // Endpoint columns captured from the PREPARED links table are authoritative: they are the same
  // rows, in the same order, as the link-colour buffer written below. `cosmos.graph.links` is the
  // renderer's own flattened pair array and is only a fallback. The previous per-index helper
  // consulted `cosmos.graph.links` FIRST and silently fell back per link, so whenever that array
  // was shorter than the link count it resolved early indices in one index space and later ones
  // in another — hiding the wrong edges and leaving visibly dangling links (operator QA
  // 2026-09-10).
  if (graphLinkEnds && graphLinkEnds.source.length >= linkCount) return graphLinkEnds;
  const pairs = cosmos?.graph?.links;
  if (pairs && pairs.length >= 2 * linkCount) {
    const source = new Array(linkCount);
    const target = new Array(linkCount);
    for (let i = 0; i < linkCount; i++) {
      source[i] = pairs[2 * i];
      target[i] = pairs[2 * i + 1];
    }
    return { source, target };
  }
  return null;
}

// The current override state — recomputed into full arrays on every change so that hide/dim,
// hue and size compose (a hue override on a hidden point stays hidden; Reset clears all three).
let overrideHidden = new Set();
let overrideDimmed = new Set();
let overrideSizes = new Map(); // point index -> size

/** Recomputes point colours, link colours and point sizes from base + overrides, then renders. */
// ─── Canvas label filter (DOM-only) ──────────────────────────────────────────────────────────
//
// Cosmograph draws point labels from its OWN duckdb-backed queries and never consults the point
// colour buffer, so a point hidden by `graphSetPointOpacity` (alpha 0) keeps its label floating
// over empty canvas. Its only built-in filter is `showLabelsFor`, reachable solely through
// `setConfig` — and `setConfig` resolves against the library defaults and deep-compares the
// points/links tables, which on 2026-09-10 dropped the colour mapping (monochrome canvas) and
// deadlocked the tab's main thread inside duckdb-wasm. Reverted in f8ed2f43.
//
// This filter therefore never touches Cosmograph state. It hides the label ELEMENTS in the DOM,
// which cannot re-upload data, cannot reach duckdb, and cannot block the main thread.
const LABEL_SELECTOR = '[class*="cosmographStaticLabel"],[class*="cosmographDynamicLabel"]';
let graphContainer = null;
let graphIdToName = null;
let labelVisibleNames = null; // Set of visible label texts, or null for "no filter"
let labelObserver = null;

// Label text is the node NAME, so the id set the Rust side owns has to be mapped through the
// prepared points table. Read with `.get(i)` rather than `.toArray()`: Arrow string columns are
// vectors, not typed arrays.
function captureIdToName(pointsTable) {
  try {
    const idCol = pointsTable?.getChild?.('id');
    const nameCol = pointsTable?.getChild?.('name');
    const rows = pointsTable?.numRows ?? 0;
    if (idCol && nameCol && rows) {
      const map = new Map();
      for (let i = 0; i < rows; i++) map.set(String(idCol.get(i)), String(nameCol.get(i)));
      return map;
    }
    console.warn('[cosmograph-graph] points table carries no id/name columns — labels will not follow point hide');
  } catch (err) {
    console.warn('[cosmograph-graph] could not read id/name columns:', err);
  }
  return null;
}

function applyLabelFilter() {
  if (!graphContainer) return;
  for (const el of graphContainer.querySelectorAll(LABEL_SELECTOR)) {
    if (!labelVisibleNames) {
      el.style.removeProperty('display');
      continue;
    }
    el.style.display = labelVisibleNames.has((el.textContent || '').trim()) ? '' : 'none';
  }
}

export function graphSetVisibleLabels(visibleIds) {
  if (!graphContainer) return;
  if (!visibleIds || visibleIds.length === 0) {
    labelVisibleNames = null;
    if (labelObserver) {
      labelObserver.disconnect();
      labelObserver = null;
    }
    applyLabelFilter();
    return;
  }
  const names = new Set();
  for (const id of visibleIds) {
    const name = graphIdToName?.get(id);
    if (name !== undefined) names.add(name);
  }
  labelVisibleNames = names;
  if (!labelObserver) {
    // childList ONLY. Cosmograph recreates label elements as the view changes, which this catches;
    // our own writes are `style` ATTRIBUTE mutations, which a childList observer cannot see — so
    // there is no feedback loop and no need for a re-entrancy guard.
    labelObserver = new MutationObserver(() => applyLabelFilter());
    labelObserver.observe(graphContainer, { childList: true, subtree: true });
  }
  applyLabelFilter();
}

function applyGraphOverrides() {
  const cosmos = ensureGraphBaseline();
  if (!cosmos) return;
  const pointCount = graphBasePointColors.length / 4;
  const colors = Float32Array.from(graphBasePointColors);
  for (let i = 0; i < pointCount; i++) {
    const o = 4 * i;
    if (overrideHidden.has(i)) {
      colors[o + 3] = 0;
      continue;
    }
    if (pointHueOverrides.has(i)) {
      const [r, g, b] = hslToRgba(
        pointHueOverrides.get(i),
        GRAPH_HUE_SATURATION,
        GRAPH_HUE_LIGHTNESS,
        1.0,
      );
      colors[o] = r / 255;
      colors[o + 1] = g / 255;
      colors[o + 2] = b / 255;
    }
    if (overrideDimmed.has(i)) colors[o + 3] = GRAPH_DIMMED_ALPHA;
  }
  cosmos.setPointColors(colors);

  if (graphBaseLinkColors) {
    const linkCount = graphBaseLinkColors.length / 4;
    const linkColors = Float32Array.from(graphBaseLinkColors);
    if (overrideHidden.size || overrideDimmed.size) {
      // Resolve the endpoint arrays ONCE, outside the loop. The previous code called a per-index
      // helper and `break`-ed the moment one index failed to resolve, which left every remaining
      // link painted at full opacity even though its endpoints were hidden — the dangling links
      // the operator saw after a shortest-paths run.
      const ends = graphLinkEndpointArrays(cosmos, linkCount);
      if (ends) {
        for (let i = 0; i < linkCount; i++) {
          const s = ends.source[i];
          const t = ends.target[i];
          if (overrideHidden.has(s) || overrideHidden.has(t)) {
            linkColors[4 * i + 3] = 0;
          } else if (overrideDimmed.has(s) || overrideDimmed.has(t)) {
            linkColors[4 * i + 3] = GRAPH_LINK_DIMMED_ALPHA;
          }
        }
      }
    }
    cosmos.setLinkColors(linkColors);
  }

  if (graphBasePointSizes) {
    const sizes = Float32Array.from(graphBasePointSizes);
    for (const [i, size] of overrideSizes) sizes[i] = size;
    cosmos.setPointSizes(sizes);
  }
  cosmos.render?.(undefined, 0);
}

/**
 * D-12/D-13: reduces every point in `dimmedIndices` to low opacity, hides every point in
 * `hiddenIndices` entirely (zero alpha), and hides/dims every link touching one — never
 * re-supplies the point/link data (D-04). Calling with two empty arrays clears both sets.
 */
export function graphSetPointOpacity(dimmedIndices, hiddenIndices) {
  overrideHidden = new Set(hiddenIndices);
  overrideDimmed = new Set(dimmedIndices);
  applyGraphOverrides();
}

/**
 * The community automatic canvas effect (plan 44-13): accumulates `hueDeg` for every index in
 * `indices` into `pointHueOverrides` — a later call for a different community does NOT erase an
 * earlier community's colouring within the same run — then re-applies. Cleared only by
 * `graphClearPointColorOverrides` (D-12's Reset path) or `graphDestroy`.
 */
export function graphSetPointColorByHue(indices, hueDeg) {
  indices.forEach((i) => pointHueOverrides.set(i, hueDeg));
  applyGraphOverrides();
}

/**
 * D-12's Reset path: clears hue overrides, dim/hide state and size overrides together and
 * restores the renderer's baseline arrays — the D-15 per-`kind` colouring Cosmograph applied at
 * mount, never a flat grey.
 */
export function graphClearPointColorOverrides() {
  pointHueOverrides.clear();
  overrideHidden = new Set();
  overrideDimmed = new Set();
  overrideSizes = new Map();
  applyGraphOverrides();
}

/**
 * The betweenness automatic canvas effect (plan 44-13): applies pre-computed per-point sizes —
 * `indices` and `scores` are parallel arrays. The 1x-3x scale computation itself lives in
 * `graph_status_line.rs`'s `betweenness_resize_scale`; this only applies pre-computed sizes.
 */
export function graphSetPointSizeByScore(indices, scores) {
  overrideSizes = new Map(indices.map((index, k) => [index, scores[k]]));
  applyGraphOverrides();
}


/** D-14: fits the viewport to a result's indices (duration/padding matching `spikeFitView`'s
 * existing constants). */
export function graphFitViewByIndices(indices) {
  graphInstance?.fitViewByIndices(indices, 500, 0.1);
}

/** Selects points by index, also selecting their connected points. */
export function graphSelectPoints(indices) {
  graphInstance?.selectPoints(indices, false, true);
}

/** D-11: the warm-up-end freeze (3s or alpha decay, whichever first) calls this — backgrounds
 * the simulation without resetting it. NOT used by Re-layout — see `graphStop` below. */
export function graphPause() {
  graphInstance?.pause();
}

/**
 * D-11: fully halts the simulation. Re-layout calls this immediately followed by `graphStart` —
 * there is no `restart()` method on Cosmograph 2.5.1 (phase 44 research Pitfall 5). Distinct from
 * `graphPause`: `stop()` fully halts the simulation so the following `start(alpha)` gives a
 * genuine fresh warm-up from full energy, matching Cosmograph's confirmed `stop()`/`start()` pair.
 */
export function graphStop() {
  graphInstance?.stop();
}

/** D-11: (re)starts the simulation at the given alpha (energy). */
export function graphStart(alpha) {
  graphInstance?.start(alpha);
}

/**
 * Tears down the mounted Graph page instance — WebGL context, simulation, event listeners — and
 * invalidates any `mountGraph` still awaiting its data. Idempotent; safe to call with nothing
 * mounted. Clears `pointHueOverrides` AND `graphKindColorMap` so neither a stale hue override nor
 * a stale (possibly wrong-theme) kind-colour map survives into a fresh mount. Called from the
 * Rust side's `Drop` (`cosmograph_trait.rs`'s `CosmographHandle`), so a rejection from
 * `destroy()` is logged here rather than thrown back across the wasm boundary.
 */
export function graphDestroy() {
  graphGeneration++;
  pointHueOverrides.clear();
  graphKindColorMap.clear();
  graphEdgeColorMap.clear();
  if (graphWarmUpTimer !== null) {
    clearTimeout(graphWarmUpTimer);
    graphWarmUpTimer = null;
  }
  graphLinkEnds = null;
  graphIdToName = null;
  labelVisibleNames = null;
  if (labelObserver) {
    labelObserver.disconnect();
    labelObserver = null;
  }
  graphContainer = null;
  graphBasePointColors = null;
  graphBasePointSizes = null;
  graphBaseLinkColors = null;
  overrideHidden = new Set();
  overrideDimmed = new Set();
  overrideSizes = new Map();
  const current = graphInstance;
  graphInstance = null;
  if (current) {
    Promise.resolve(current.destroy()).catch((err) =>
      console.error('[cosmograph-graph] destroy failed:', err),
    );
  }
}
