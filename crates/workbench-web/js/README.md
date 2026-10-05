# Vendored Cosmograph shim

`cosmograph-shim.js` is a **vendored, pinned ESM bundle**, not an npm dependency the workbench
fetches at build or run time (Phase 42, D-16, WBX-07). It is produced once by a local, explicit
build step and committed to this directory; `crates/workbench-web/src/cosmograph_spike.rs`'s
`#[wasm_bindgen(module = "/js/cosmograph-shim.js")]` binding loads it directly.

**Phase 44 (D-17)** extended the bundle with the Graph page's mount/highlight/dim/hide/fit/
re-layout/select/destroy surface — `mountGraph`, `graphSetPointOpacity`,
`graphSetPointColorByHue`, `graphClearPointColorOverrides`, `graphSetPointSizeByScore`,
`graphFitViewByIndices`, `graphSelectPoints`, `graphPause`, `graphStop`, `graphStart`,
`graphDestroy` — alongside the Phase 42 spike's original `mountSpike`/`spikeFitView`/
`spikeSelectPoint`/`spikeDestroy` exports (kept, unchanged, for the `/spike/cosmograph` route).
The Graph page surface is consumed exclusively through
`crates/workbench-web/src/cosmograph_trait.rs`'s `CosmographHandle` (D-17: no page or algorithm
code calls these exports directly).

**Plan 44-16** closed a cross-plan gap plan 44-10 had flagged: `mountGraph(container, nodesBytes,
linksBytes, kindColorEntries, onNodeClick)` now takes the nodes and links Arrow IPC streams as
TWO SEPARATE arguments (matching `prepareCosmographData(config, pointsData, linksData?)`'s own
separate points/links parameters), so edges draw on the canvas, and `kindColorEntries` (flat
`[kind, cssVarName]` pairs from `node_palette.rs`) is resolved to concrete colours at mount time
and wired into `pointColorBy: 'kind'` + `pointColorByMap`, so every point is coloured by its
`kind` column from the moment it mounts (D-15). `graphClearPointColorOverrides` (D-12's Reset
path) now restores this per-kind colouring rather than a flat grey.

## Location — why this directory, and why not a symlink

`wasm-bindgen`'s local-module attribute resolves a leading-`/` path against the **binding
crate's own** `CARGO_MANIFEST_DIR` (`crates/workbench-web/`) and reads the whole file at macro
expansion time — on **every** target, including the host build `cargo build --workspace` /
`cargo test --workspace` perform. The bundle therefore has to be physically reachable from this
crate. It was originally vendored into `crates/workbench/js/` and bridged with a relative symlink
`crates/workbench-web/js -> ../workbench/js`; a checkout without symlink support (Windows'
default `core.symlinks=false` materialises the link as a plain text file containing
`../workbench/js`) then broke the entire workspace build, not just the wasm target (Phase 42
code review, IN-06). The bundle now lives here as a real directory and **no symlink is
involved**. Nothing else reads it: Trunk only processes `data-trunk` links in `index.html`, and
`crates/workbench`'s `rust-embed` embeds `dist/`, not `js/`.

## Pinned version

- **Package:** `@cosmograph/cosmograph`
- **Version:** `2.5.1` (confirmed live on the npm registry via `npm view @cosmograph/cosmograph
  version` on 2026-09-04 — matches the milestone/phase research's carried-over figure exactly, no
  drift)
- **Bundler:** `esbuild` (devDependency pinned `^0.25.0` in
  `crates/workbench-web/js-build/package.json`)

## Integrity

```
7eadbf60dfe3c6e7ae94f9705f52be7a362246e1e34b06992c792c43b0b88290  cosmograph-shim.js
```

Verify with `sha256sum -c crates/workbench-web/js/cosmograph-shim.js.sha256` (run from this
directory, or `cd crates/workbench-web/js && sha256sum -c cosmograph-shim.js.sha256`).

## Licence

`@cosmograph/cosmograph` **2.5.1** is licensed **CC-BY-NC-4.0** (non-commercial) — acceptable for
this local, single-operator research tool with no commercial use (phase 44 design decision D-16). This
wording is kept word-for-word consistent with the Help page's own licence line
(`crates/workbench-web/src/help/mod.rs::COSMOGRAPH_LICENCE_LINE`, added by plan 44-10 per
the phase 44 UI spec's "Help page licence line" contract — the two must state the same facts: package,
pinned version, licence, non-commercial acceptability, even though their surrounding prose
differs, one being developer documentation and the other analyst-facing copy). This is a **HARD
CONFLICT** with any future commercial use of the workbench and MUST be revisited (commercial
licence purchase or a different renderer) before that happens — see
the phase 42 design notes' Deferred Ideas ("Monetisation").

## Regenerating the bundle (explicit, reviewed step — never automatic)

No Node, npm, or wasm toolchain is required for a normal `cargo build`/`cargo test`/`cargo
clippy` run — this bundle is committed precisely so the workspace never needs Node at build time
(D-16). Regeneration is an explicit, human-reviewed upgrade step only:

```bash
cd crates/workbench-web/js-build
npm install
npx esbuild cosmograph-shim-src.js --bundle --format=esm \
  --outfile=../js/cosmograph-shim.js
sha256sum cosmograph-shim.js > cosmograph-shim.js.sha256   # run from crates/workbench-web/js
```

After regenerating: re-verify the pinned `@cosmograph/cosmograph` version against
`npm view @cosmograph/cosmograph version`, update the "Pinned version" and "Integrity" sections
of this README with the new version/SHA-256, and manually check the workbench graph page
(`/graph`) renders and responds before trusting the new bundle.

`crates/workbench-web/js-build/node_modules/` is gitignored — never committed.
`crates/workbench-web/js-build/package.json`/`package-lock.json` ARE committed, for
reproducibility of this one-shot build tool.
