#!/usr/bin/env bash
#
# export_graphml.sh — export the live UrbanIntegrity graph (read from the
# `graph_events` WAL in SurrealDB) to a timestamped GraphML file in the repo root.
#
# Output: <repo-root>/urban-integrity-<YYYYMMDD-HHMMSS>.graphml
#
# This wraps the `urban-integrity export-graphml --source events` CLI subcommand,
# which reconstructs nodes (NodeAdded) and edges (EdgeAdded) directly from
# graph_events — no `urban-integrity project` step required.
#
# Usage:
#   scripts/export_graphml.sh            # build (if needed) + export
#   scripts/export_graphml.sh --debug    # use the debug build instead of release
#
# Env:
#   URBAN_CLI   override the CLI binary path (skips cargo build entirely)
#
set -euo pipefail

# ── Resolve repo root from this script's location (robust to CWD) ────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Parse args ───────────────────────────────────────────────────────────────
PROFILE="release"
CARGO_PROFILE_FLAG="--release"
for arg in "$@"; do
  case "$arg" in
    --debug)
      PROFILE="debug"
      CARGO_PROFILE_FLAG=""
      ;;
    -h|--help)
      sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

# ── Timestamped output path ──────────────────────────────────────────────────
STAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT="$REPO_ROOT/urban-integrity-${STAMP}.graphml"

# ── Locate or build the CLI binary ───────────────────────────────────────────
# The workspace binary is named `urban-integrity-cli` (see crates/cli/Cargo.toml).
BIN="${URBAN_CLI:-$REPO_ROOT/target/$PROFILE/urban-integrity-cli}"

if [[ -n "${URBAN_CLI:-}" ]]; then
  echo "Using CLI from URBAN_CLI=$BIN"
elif [[ -x "$BIN" ]]; then
  echo "Using existing $PROFILE build: $BIN"
else
  echo "Building urban-integrity-cli ($PROFILE)…"
  # shellcheck disable=SC2086
  cargo build -p urban-integrity-cli $CARGO_PROFILE_FLAG
fi

if [[ ! -x "$BIN" ]]; then
  echo "error: CLI binary not found at $BIN" >&2
  exit 1
fi

# ── Export ───────────────────────────────────────────────────────────────────
echo "Exporting graph_events → GraphML…"
"$BIN" export-graphml --source events --output "$OUTPUT"

# `export-graphml` prints its own "Written → …" line; confirm + summarise here.
if [[ -f "$OUTPUT" ]]; then
  SIZE="$(du -h "$OUTPUT" | cut -f1)"
  echo "✓ GraphML written: $OUTPUT ($SIZE)"
else
  echo "error: expected output file was not created: $OUTPUT" >&2
  exit 1
fi
