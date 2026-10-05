#!/usr/bin/env bash
# scripts/enrich.sh — Run OSINT enrichment adapters against the live graph.
#
# Usage:
#   ./scripts/enrich.sh [--source all|london_gazette|land_registry_ccod|...]
#                        [--no-db]
#
# Flags:
#   --source <name>   Adapter to run (default: all). 'all' runs every enabled adapter.
#   --no-db           Skip SurrealDB start/stop — use when DB is already running.
#
# Prerequisites:
#   - Rust toolchain installed (cargo)
#   - SurrealDB v3 at ~/.surrealdb/surreal  (not needed with --no-db)
#
# Lookback note:
#   The London Gazette adapter has no deep backfill. compute_published_from()
#   starts an empty cursor at (today - lookback_days), default 30, and clamps an
#   older cursor to that same floor — so the first run covers the same span as
#   every later one. The 2020-01-01 backfill this note used to describe was
#   removed by the runaway-crawl fix (d12a281, 2026-06-09).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SURREAL_BIN="${HOME}/.surrealdb/surreal"
# Storage backend + DB path are owned by scripts/surreal-start.sh (rocksdb://).
SURREAL_BIND="[::]:8000"
SURREAL_URL="ws://localhost:8000"

# ── Colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info()  { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()   { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

# ── Argument parsing ──────────────────────────────────────────────────────────
SOURCE="all"
NO_DB=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --source) SOURCE="$2"; shift 2 ;;
        --no-db)  NO_DB=true; shift ;;
        -h|--help)
            echo "Usage: $0 [--source all|<adapter>] [--no-db]"
            echo "  --source  Adapter name or 'all' (default: all)"
            echo "  --no-db   Skip SurrealDB lifecycle management"
            exit 0 ;;
        *) die "Unknown argument: $1. Run with --help for usage." ;;
    esac
done

# ── Preflight ─────────────────────────────────────────────────────────────────
command -v cargo &>/dev/null || die "cargo not found. Install Rust from https://rustup.rs"
if [[ "$NO_DB" = false ]]; then
    [[ -x "$SURREAL_BIN" ]] || die "SurrealDB not found at $SURREAL_BIN. Install from https://surrealdb.com/install"
fi

# ── Cleanup on exit ───────────────────────────────────────────────────────────
# SurrealDB now runs as the isolated, memory-capped 'urban-surreal' user service
# (see scripts/surreal-start.sh) so a DB OOM can never take the editor down.
STARTED_SURREAL=false
cleanup() {
    echo ""
    if [[ "$STARTED_SURREAL" = true ]]; then
        info "Stopping SurrealDB (user service urban-surreal)..."
        systemctl --user stop urban-surreal 2>/dev/null || true
        sleep 1
        rm -f "$WORKSPACE_ROOT/data/surreal.db/LOCK"
    fi
}
if [[ "$NO_DB" = false ]]; then
    trap cleanup EXIT INT TERM
fi

# ── Start SurrealDB (unless --no-db) ─────────────────────────────────────────
cd "$WORKSPACE_ROOT"
mkdir -p data

if [[ "$NO_DB" = false ]]; then
    # Launch isolated + memory-capped (protects the editor from a DB OOM).
    "$SCRIPT_DIR/surreal-start.sh" || die "Failed to start isolated SurrealDB. Check data/surreal.log."
    STARTED_SURREAL=true
else
    info "--no-db: skipping SurrealDB startup (assuming it is already running)"
fi

# ── Build ─────────────────────────────────────────────────────────────────────
info "Building urban-integrity-cli..."
cargo build -p urban-integrity-cli -q

# ── Run enrichment ────────────────────────────────────────────────────────────
echo ""
info "Running enrichment — source: ${SOURCE}"
if [[ "$SOURCE" == "london_gazette" ]] || [[ "$SOURCE" == "all" ]]; then
    warn "London Gazette runs a ${GAZETTE_LOOKBACK:-30}-day lookback under a max_pass_secs budget"
    warn "(default 300s). A pass that exceeds the budget checkpoints mid-crawl and returns;"
    warn "re-run to resume from the checkpoint. Draining a backlog takes several passes."
fi
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

URBAN__SURREAL__WS_URL="$SURREAL_URL" \
cargo run -q -p urban-integrity-cli -- enrich --source "$SOURCE"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
ok "Enrichment complete."
