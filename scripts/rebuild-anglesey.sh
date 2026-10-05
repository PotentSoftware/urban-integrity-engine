#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# scripts/rebuild-anglesey.sh — refresh the analysis for the 14 Anglesey wards
# WITHOUT re-fetching external data. Keeps all collected Companies House / Land
# Registry / enrichment events in graph_events; deletes only the broken derived
# layer (motif alerts + CCOD orphans) and regenerates it with the fixed detector.
#
# MAINTAINER-SPECIFIC: written for the maintainers' own reference analysis; not a
# general-purpose tool or an example to copy for your own analysis.
#
# See the refresh-analysis runbook in the project's planning notes for the rationale
# and manual steps.
#
# Usage:
#   scripts/rebuild-anglesey.sh                 # full run (prompts before deleting)
#   scripts/rebuild-anglesey.sh --yes           # no prompts
#   scripts/rebuild-anglesey.sh --skip-backup   # skip the surreal export
#   scripts/rebuild-anglesey.sh --skip-enrich   # skip OSINT enrichment
#   scripts/rebuild-anglesey.sh --skip-prune    # derived layer already pruned; resume at step 3
#   scripts/rebuild-anglesey.sh --detect-secs 180 --ward W05001501
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

SURREAL_BIN="${SURREAL_BIN:-${HOME}/.surrealdb/surreal}"
ENDPOINT="http://localhost:8000"
NS="urban_integrity"; DB=""   # resolved after arg parsing
CLI=""        # set once DB is known, so the CLI and raw SQL agree

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
step()  { echo -e "\n${CYAN}━━ $* ━━${RESET}"; }
ok()    { echo -e "${GREEN}[ ok ]${RESET} $*"; }
warn()  { echo -e "${YELLOW}[warn]${RESET} $*"; }
die()   { echo -e "${RED}[fail]${RESET} $*" >&2; exit 1; }

ASSUME_YES=false; SKIP_BACKUP=false; SKIP_ENRICH=false; SKIP_PRUNE=false; DETECT_SECS=90; REPORT_WARD="W05001497"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --yes) ASSUME_YES=true; shift ;;
    --skip-backup) SKIP_BACKUP=true; shift ;;
    --skip-enrich) SKIP_ENRICH=true; shift ;;
    --skip-prune) SKIP_PRUNE=true; shift ;;
    --detect-secs) DETECT_SECS="$2"; shift 2 ;;
    --ward) REPORT_WARD="$2"; shift 2 ;;
    --analysis) DB="$2"; shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "Unknown arg: $1 (try --help)" ;;
  esac
done

# ── Analysis target ──────────────────────────────────────────────────────────
# Was hardcoded DB="main" — a pre-lifecycle database that no CLI command reads,
# so the raw-SQL half of this script (backup, prune, curl) operated on `main`
# while every $CLI call used config.toml's analysis. Both halves now agree.
# Precedence matches the Rust CLI: --analysis > $URBAN__ANALYSIS > config file, where the
# config file is config.local.toml if it exists, else config.toml (AppConfig::load()'s choice).
CONFIG_FILE="$ROOT/config.local.toml"
[[ -f "$CONFIG_FILE" ]] || CONFIG_FILE="$ROOT/config.toml"
if [[ -z "$DB" ]]; then DB="${URBAN__ANALYSIS:-}"; fi
if [[ -z "$DB" ]]; then
  DB="$(sed -n '/^\[surreal\]/,/^\[/p' "$CONFIG_FILE" \
        | sed -n 's/^database *= *"\(.*\)"/\1/p' | head -1)"
fi
[[ -n "$DB" ]] || die "no analysis selected — pass --analysis, set URBAN__ANALYSIS, or add [surreal].database to config.local.toml"
CLI="cargo run -q -p urban-integrity-cli -- --analysis $DB"

confirm() { $ASSUME_YES && return 0; read -r -p "$1 [y/N] " a; [[ "$a" == "y" || "$a" == "Y" ]]; }
surreal_sql() { echo "$1" | "$SURREAL_BIN" sql --endpoint "$ENDPOINT" -u root -p root --namespace "$NS" --database "$DB"; }

# ── Preflight ────────────────────────────────────────────────────────────────
command -v cargo >/dev/null || die "cargo not found — 'source \$HOME/.cargo/env'"
[[ -x "$SURREAL_BIN" ]] || die "surreal not found at $SURREAL_BIN"

step "Preflight"
if ! curl -sf --max-time 3 "$ENDPOINT/health" >/dev/null 2>&1 \
   && ! curl -sf --max-time 3 "http://[::1]:8000/health" >/dev/null 2>&1; then
  warn "SurrealDB not up — starting it"; "$SCRIPT_DIR/surreal-start.sh" || die "surreal-start failed"
fi
ok "SurrealDB reachable"
curl -sf --max-time 3 "http://localhost:8765/health" >/dev/null 2>&1 \
  && ok "Scrapling sidecar up (:8765)" \
  || warn "Scrapling sidecar not detected on :8765 — planning scrape may return nothing"
[[ -n "${URBAN__INGEST__CH_API_KEY:-}" ]] \
  && ok "CH API key present (backfill ch enabled)" \
  || warn "URBAN__INGEST__CH_API_KEY unset — 'backfill ch' will be skipped"

WARDS=$(grep -oE 'W0500149[2-9]|W050015[0-9][0-9]' "$CONFIG_FILE" | sort -u | wc -l | tr -d ' ')
[[ "$WARDS" == "14" ]] || warn "Expected 14 Anglesey wards in $(basename "$CONFIG_FILE"), found $WARDS"

# ── 0. Backup ────────────────────────────────────────────────────────────────
if ! $SKIP_BACKUP; then
  step "0. Backup export (insurance)"
  BK="./data/backup-$(date +%Y%m%d-%H%M%S).surql"
  "$SURREAL_BIN" export --endpoint "$ENDPOINT" -u root -p root --namespace "$NS" --database "$DB" "$BK" \
    && ok "Backed up → $BK" || warn "Export failed (check 'surreal export --help' flags) — continuing"
fi

# ── 1. Prune the derived layer (DESTRUCTIVE) ─────────────────────────────────
# A single `DELETE graph_events WHERE kind = 'MotifDetected'` never converges at this
# row count — it re-scans and commits nothing (observed: 30 min, 1.9 TB read, 110 MB
# written, zero rows removed). Delete by explicit id list instead, in batches.
# Batches also degrade over time as RocksDB tombstones pile up in the memtables and the
# indexed scan has to skip every deleted key; restarting SurrealDB flushes them and
# restores millisecond latency, so slow batches trigger a restart.
prune_motifs() {
  local batch=20000 slow=20 total=0 t0 elapsed n resp
  while :; do
    t0=$SECONDS
    resp=$(curl -s -X POST "$ENDPOINT/sql" -u root:root \
      -H "surreal-ns: $NS" -H "surreal-db: $DB" -H "Accept: application/json" --max-time 300 \
      -d "LET \$ids = (SELECT VALUE id FROM graph_events WHERE kind = 'MotifDetected' LIMIT ${batch}); DELETE \$ids; RETURN count(\$ids);")
    elapsed=$((SECONDS - t0))
    n=$(echo "$resp" | grep -oE '"result":[0-9]+' | tail -1 | grep -oE '[0-9]+$')
    if [[ -z "${n:-}" ]]; then
      warn "prune batch stalled after ${elapsed}s — flushing SurrealDB and retrying"
      systemctl --user stop urban-surreal; sleep 5
      "$SCRIPT_DIR/surreal-start.sh" >/dev/null 2>&1 || die "surreal-start failed mid-prune"
      sleep 2; continue
    fi
    total=$((total + n))
    [[ "$n" -eq 0 ]] && break
    if [[ "$elapsed" -ge "$slow" ]]; then
      warn "batch took ${elapsed}s (tombstone build-up) — flushing SurrealDB"
      systemctl --user stop urban-surreal; sleep 5
      "$SCRIPT_DIR/surreal-start.sh" >/dev/null 2>&1 || die "surreal-start failed mid-prune"
      sleep 2
    fi
  done
  ok "MotifDetected alerts deleted ($total rows)"
}

if $SKIP_PRUNE; then
  step "1. Prune — SKIPPED (--skip-prune)"
elif confirm "Delete all MotifDetected alerts and purge CCOD orphans? (raw ingestion events are kept)"; then
  step "1. Prune broken derived layer (motif alerts + CCOD orphans)"
  prune_motifs
  # NOTE: purge-orphans' --apply delete path is currently a silent no-op, so this line
  # has no effect today. Left in deliberately — but if --apply is ever fixed, re-verify
  # the orphan definition against live data before trusting it here.
  $CLI purge-orphans --apply || warn "purge-orphans reported an issue"
else
  warn "Skipped prune — alerts will be stale/duplicated. Continuing anyway."
fi

# ── 2. Config already trimmed to the 14 wards (no action) ────────────────────

# ── 3. Ingest planning (14 wards) ────────────────────────────────────────────
step "3. Ingest planning — 14 Anglesey wards"
$CLI ingest-planning --all || warn "ingest-planning reported an issue — check the per-ward counts above"

# ── 4. Targeted CH + ward backfill ───────────────────────────────────────────
step "4. Backfill (CH numbers, ward codes)"
if [[ -n "${URBAN__INGEST__CH_API_KEY:-}" ]]; then
  $CLI backfill ch || warn "backfill ch reported an issue"
else
  warn "Skipping 'backfill ch' — no CH API key"
fi
$CLI backfill ward || warn "backfill ward reported an issue"

# ── 5. Enrichment + projection ───────────────────────────────────────────────
if ! $SKIP_ENRICH; then
  step "5. Enrichment (OSINT sources)"
  "$SCRIPT_DIR/enrich.sh" --source all || warn "enrich reported an issue"
fi
step "5b. Project WAL → typed tables"
$CLI project || warn "project reported an issue"

# ── 6. Detect (CH firehose OFF) ──────────────────────────────────────────────
step "6. Detect — serve with CH streaming disabled (${DETECT_SECS}s)"
cargo build -q -p urban-integrity-cli || die "build failed"
BIN="$ROOT/target/debug/urban-integrity-cli"
env -u URBAN__INGEST__CH_API_KEY "$BIN" serve &>"$ROOT/data/serve-detect.log" &
SERVE_PID=$!
trap '[[ -n "${SERVE_PID:-}" ]] && kill "$SERVE_PID" 2>/dev/null' EXIT INT TERM
ok "serve running (PID $SERVE_PID) — replaying + detecting; log: data/serve-detect.log"
sleep "$DETECT_SECS"
kill "$SERVE_PID" 2>/dev/null; wait "$SERVE_PID" 2>/dev/null; SERVE_PID=""
ok "detection window elapsed; engine stopped"

# ── 7. Report ────────────────────────────────────────────────────────────────
step "7. Report — ward $REPORT_WARD"
$CLI report --ward "$REPORT_WARD" || warn "report reported an issue"

echo
surreal_sql "SELECT kind, count() FROM graph_events GROUP BY kind;" || true
echo -e "\n${GREEN}Done.${RESET} Verify the MotifDetected count is sane (not millions) and that"
echo "report --ward <code> returns alerts carrying that ward. See the refresh-analysis runbook."
