#!/usr/bin/env bash
# scripts/report.sh — End-to-end report runner for UrbanIntegrity Engine
#
# Usage:
#   ./scripts/report.sh --ward X05999001 [--min-score 0.7] [--output json]
#
# Prerequisites:
#   - URBAN__INGEST__CH_API_KEY must be set in your environment
#   - Rust toolchain installed (cargo)
#   - SurrealDB v3 installed at ~/.surrealdb/surreal (or set SURREAL_BIN, as for
#     scripts/surreal-start.sh)
#
# The script:
#   1. Starts SurrealDB in the background
#   2. Builds and starts the engine (serve) in the background
#   3. Waits for both services to be ready
#   4. Runs the report command with --ward <arg>
#   5. Shuts down SurrealDB and the engine on exit (Ctrl-C or completion)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SURREAL_BIN="${SURREAL_BIN:-${HOME}/.surrealdb/surreal}"
export SURREAL_BIN  # scripts/surreal-start.sh reads the same variable
# Storage backend + DB path are owned by scripts/surreal-start.sh (rocksdb://).
# Bind to all interfaces (IPv4 + IPv6) so the surrealdb v1 SDK can connect
# via the IPv6 loopback (::1) that getaddrinfo returns for 'localhost' on this system.
SURREAL_BIND="[::]:8000"
SURREAL_URL="ws://localhost:8000"

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info()  { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()   { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

# ── Argument parsing ──────────────────────────────────────────────────────────
WARD=""
MIN_SCORE="0.7"
OUTPUT_FORMAT="table"

usage() {
    echo "Usage: $0 --ward <ward-code> [--min-score <0.0-1.0>] [--output table|json]"
    echo ""
    echo "  --ward       ONS ward code to scope the report (required), e.g. X05999001"
    echo "  --min-score  Minimum risk score threshold (default: 0.7)"
    echo "  --output     Output format: table or json (default: table)"
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ward)       WARD="$2";         shift 2 ;;
        --min-score)  MIN_SCORE="$2";    shift 2 ;;
        --output)     OUTPUT_FORMAT="$2"; shift 2 ;;
        -h|--help)    usage ;;
        *) die "Unknown argument: $1. Run with --help for usage." ;;
    esac
done

[[ -z "$WARD" ]] && { warn "No --ward argument provided."; usage; }

# ── Preflight checks ──────────────────────────────────────────────────────────
[[ -x "$SURREAL_BIN" ]] || die "SurrealDB not found at $SURREAL_BIN. Install from https://surrealdb.com/install"
command -v cargo &>/dev/null || die "cargo not found. Install Rust from https://rustup.rs"

if [[ -z "${URBAN__INGEST__CH_API_KEY:-}" ]]; then
    warn "URBAN__INGEST__CH_API_KEY is not set — Companies House streaming will be disabled."
fi

# ── Cleanup on exit ───────────────────────────────────────────────────────────
# SurrealDB now runs as the isolated, memory-capped 'urban-surreal' user service
# (see scripts/surreal-start.sh) so a DB OOM can never take the editor down.
STARTED_SURREAL=false
SERVE_PID=""

cleanup() {
    echo ""
    info "Shutting down..."
    [[ -n "$SERVE_PID" ]] && kill "$SERVE_PID" 2>/dev/null && info "Engine stopped (PID $SERVE_PID)"
    if [[ "$STARTED_SURREAL" = true ]]; then
        systemctl --user stop urban-surreal 2>/dev/null && info "SurrealDB stopped (user service urban-surreal)"
    fi
    # Allow clean shutdown before force-removing the lock
    sleep 1
    rm -f "$WORKSPACE_ROOT/data/surreal.db/LOCK"
}
trap cleanup EXIT INT TERM

# ── Step 1: Start SurrealDB ───────────────────────────────────────────────────
cd "$WORKSPACE_ROOT"
mkdir -p data

# Launch isolated + memory-capped (protects the editor from a DB OOM).
"$SCRIPT_DIR/surreal-start.sh" || die "Failed to start isolated SurrealDB. Check data/surreal.log."
STARTED_SURREAL=true

# (legacy readiness loop retained below as a no-op safety net)
info "Confirming SurrealDB readiness..."
for i in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:8000/health" &>/dev/null || curl -sf "http://[::1]:8000/health" &>/dev/null; then
        ok "SurrealDB is ready (${i}×500ms)"
        break
    fi
    sleep 0.5
    if [[ $i -eq 30 ]]; then
        die "SurrealDB did not start within 15 s. Check data/surreal.log for details."
    fi
done

# ── Step 2: Build + Start the engine ─────────────────────────────────────────
info "Building urban-integrity-cli..."
cargo build -p urban-integrity-cli -q

info "Starting engine (serve)..."
URBAN__SURREAL__WS_URL="$SURREAL_URL" \
cargo run -q -p urban-integrity-cli -- serve \
    &>"${WORKSPACE_ROOT}/data/serve.log" &
SERVE_PID=$!

# Give the engine time to initialise the graph coordinator + API server
info "Waiting for engine to initialise (5 s)..."
sleep 5

# Verify the engine didn't crash immediately
if ! kill -0 "$SERVE_PID" 2>/dev/null; then
    echo ""
    warn "Engine log (data/serve.log):"
    echo "────────────────────────────────────────────────────────────────────"
    tail -30 "${WORKSPACE_ROOT}/data/serve.log" 2>/dev/null || echo "  (log is empty)"
    echo "────────────────────────────────────────────────────────────────────"
    die "Engine exited prematurely — see log above."
fi
ok "Engine is running (PID $SERVE_PID)"

# ── Step 3: Generate the report ───────────────────────────────────────────────
echo ""
info "Generating report — ward: ${WARD}, min-score: ${MIN_SCORE}, format: ${OUTPUT_FORMAT}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

URBAN__SURREAL__WS_URL="$SURREAL_URL" \
cargo run -q -p urban-integrity-cli -- report \
    --ward "$WARD" \
    --min-score "$MIN_SCORE" \
    --output "$OUTPUT_FORMAT"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
ok "Report complete."
