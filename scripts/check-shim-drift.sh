#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-shim-drift.sh — does cosmograph-shim-src.js still generate the bundle?
#
# crates/workbench-web/js/cosmograph-shim.js is the VENDORED, committed ESM
# bundle the wasm_bindgen `module = "/js/cosmograph-shim.js"` bindings load.
# crates/workbench-web/js-build/cosmograph-shim-src.js is documented as its
# source. Between 2026-09-05 and 2026-09-13 that was false: four features
# (pointLabelBy, graphEdgeColorMap/linkColorByFn, showHoveredPointLabel and
# D-11's armWarmUpFreeze) had been edited straight into the bundle by 278a4a38
# and c2f344eb and never written back, so `npm run build` would have silently
# reverted all four. This script is what makes that detectable in one command.
#
# It rebuilds the source to a temp file and compares only the shim section of
# each bundle — the ~410 lines after esbuild's `// cosmograph-shim-src.js`
# banner — after normalising away everything esbuild is free to change and the
# hand-edits never went through:
#
#   * comments and blank lines (esbuild keeps some leading comments and drops
#     others; a hand-typed comment always survives, so presence is not a fact
#     about the source)
#   * /* @__PURE__ */ annotations and —-style escapes
#   * esbuild's collision-avoidance suffixes on OUR identifiers (i -> i8,
#     pairs -> pairs3, map -> map6, ...), which appear only where the committed
#     bundle was typed in by hand rather than generated
#   * the order of the `export { ... }` list, which is a set
#
# Anything left is a real semantic difference between the source and what
# ships. Exit 0 = the source regenerates the bundle; exit 1 = drift.
#
# Usage:   scripts/check-shim-drift.sh
#
# Requires crates/workbench-web/js-build/node_modules (gitignored, ~4MB);
# `cd crates/workbench-web/js-build && npm install` provisions it. Skips with
# exit 0 and a warning when it is absent, so this stays runnable on a machine
# with no Node — same contract as build-workbench.sh.
#
# SHIM_DRIFT_STRICT=1 turns that skip into a failure. CI sets it: a guard whose
# only failure mode is "the thing I check was not installed" is a green tick
# that proves nothing, which is exactly how this drift survived eight days.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="$REPO_ROOT/crates/workbench-web/js-build"
BUNDLE="$REPO_ROOT/crates/workbench-web/js/cosmograph-shim.js"

[[ -f "$BUNDLE" ]] || die "vendored bundle not found: $BUNDLE"

STRICT="${SHIM_DRIFT_STRICT:-0}"

if [[ ! -d "$BUILD_DIR/node_modules" ]]; then
  if [[ "$STRICT" == "1" ]]; then
    die "node_modules absent and SHIM_DRIFT_STRICT=1 — run 'npm ci' in $BUILD_DIR first.
        Refusing to report success for a comparison that never ran."
  fi
  warn "node_modules absent — skipping (run 'cd crates/workbench-web/js-build && npm install' to enable)"
  exit 0
fi

if ! command -v python3 >/dev/null; then
  if [[ "$STRICT" == "1" ]]; then
    die "python3 not found and SHIM_DRIFT_STRICT=1 — the comparison cannot run"
  fi
  warn "python3 not found — skipping the normalised comparison"
  exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

info "rebuilding cosmograph-shim-src.js to a temp bundle (the vendored one is NOT touched)..."
( cd "$BUILD_DIR" && npx --no-install esbuild cosmograph-shim-src.js \
    --bundle --format=esm --outfile="$TMP/rebuilt.js" >/dev/null 2>"$TMP/esbuild.log" ) \
  || { cat "$TMP/esbuild.log" >&2; die "esbuild failed"; }

status=0
python3 - "$BUNDLE" "$TMP/rebuilt.js" <<'PYEOF' || status=$?
import io, re, sys

BANNER = '// cosmograph-shim-src.js'
# esbuild appends a numeric suffix to any of our identifiers that collides with
# one in the bundled library. The committed bundle carries the unsuffixed name
# wherever a human typed the line in directly.
RENAMES = [('pairs3', 'pairs'), ('target2', 'target'), ('map6', 'map'),
           ('name3', 'name'), ('id2', 'id'), ('i8', 'i')]


def normalise(path):
    lines = io.open(path, encoding='utf-8').read().split('\n')
    try:
        start = next(i for i, l in enumerate(lines) if l.strip() == BANNER)
    except StopIteration:
        sys.exit("%s: no '%s' banner — not an esbuild bundle of this source?" % (path, BANNER))
    body = '\n'.join(lines[start:])
    body = body.replace('/* @__PURE__ */ ', '').replace('\\u2014', '—')
    for suffixed, plain in RENAMES:
        body = re.sub(r'\b%s\b' % suffixed, plain, body)
    kept = []
    for line in body.split('\n'):
        stripped = line.strip()
        if not stripped or stripped.startswith('//'):
            continue
        kept.append(re.sub(r'\s*//.*$', '', stripped))
    text = '\n'.join(kept)
    # `{ x: x }` is what the rename above leaves where the source said `{ x }`.
    text = re.sub(r'\b(\w+): \1\b', r'\1', text)
    # The export list is a set, not a sequence.
    m = re.search(r'export \{\n(.*?)\n\};', text, re.S)
    if m:
        names = sorted(n.strip().rstrip(',') for n in m.group(1).split('\n'))
        text = text[:m.start()] + 'export {\n' + '\n'.join(names) + '\n};' + text[m.end():]
    return text.split('\n')


committed, rebuilt = normalise(sys.argv[1]), normalise(sys.argv[2])
if committed == rebuilt:
    print('%d normalised lines compared' % len(committed))
    sys.exit(0)

import difflib
print('\n'.join(difflib.unified_diff(committed, rebuilt,
                                     'vendored js/cosmograph-shim.js',
                                     'rebuilt from js-build/cosmograph-shim-src.js',
                                     lineterm='', n=2)))
sys.exit(1)
PYEOF

if [[ $status -ne 0 ]]; then
  die "SHIM DRIFT: the vendored bundle is not what cosmograph-shim-src.js builds.
        A '<' line ships but is not in the source (running 'npm run build' would DELETE it);
        a '>' line is in the source but does not ship. Port the difference into the source,
        re-run this, and only then regenerate the bundle."
fi

ok "cosmograph-shim-src.js regenerates the vendored bundle — no drift"
