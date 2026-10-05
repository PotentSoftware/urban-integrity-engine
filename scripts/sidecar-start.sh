#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# sidecar-start.sh — launch the Scrapling planning sidecar the same way every time.
#
# The sidecar (scrapling-sidecar/sidecar.py) is a FastAPI/uvicorn service on
# :8765 that the Rust engine calls for planning-portal scraping (Idox, Arcus/
# Salesforce, …). Ingestion degrades gracefully to a WARN if it is down, so this
# is only needed before an `ingest-planning` run or a `serve` with planning polling.
#
# Runs the sidecar as a transient *user systemd service* (detached from the
# calling terminal), falling back to nohup if systemd-run --user is unavailable.
# Idempotent: a healthy instance already up is a no-op.
#
# Usage:  scripts/sidecar-start.sh
# Stop:   systemctl --user stop urban-sidecar   (or: kill "$(cat data/sidecar.pid)")
# Logs:   journalctl --user -u urban-sidecar -f  (or: data/sidecar.log)
# Tune:   SIDECAR_PORT=8765  URBAN_SIDECAR_PYTHON=/path/to/python
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SIDECAR_DIR="${WORKSPACE_ROOT}/scrapling-sidecar"
PORT="${SIDECAR_PORT:-8765}"
UNIT="urban-sidecar"
LOG="${WORKSPACE_ROOT}/data/sidecar.log"
PIDFILE="${WORKSPACE_ROOT}/data/sidecar.pid"
# Prefer the sidecar's own venv; fall back to system python3.
PYTHON="${URBAN_SIDECAR_PYTHON:-${SIDECAR_DIR}/.venv/bin/python}"
[[ -x "$PYTHON" ]] || PYTHON="$(command -v python3 || true)"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

health() { curl -sf "http://127.0.0.1:${PORT}/health" &>/dev/null; }

[[ -n "$PYTHON" && -x "$PYTHON" ]] || die "no usable python found (set URBAN_SIDECAR_PYTHON)."
[[ -f "${SIDECAR_DIR}/sidecar.py" ]] || die "sidecar.py not found in ${SIDECAR_DIR}"

# ── Idempotency: already healthy? ────────────────────────────────────────────
if health; then
    ok "Sidecar already running and healthy on :${PORT} — nothing to do."
    exit 0
fi

mkdir -p "${WORKSPACE_ROOT}/data"

# ── Tear down anything stale ─────────────────────────────────────────────────
if command -v systemctl &>/dev/null; then
    systemctl --user reset-failed "$UNIT" 2>/dev/null || true
    systemctl --user stop "$UNIT" 2>/dev/null || true
fi
if [[ -f "$PIDFILE" ]] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    warn "Stopping stale sidecar (pid $(cat "$PIDFILE"))..."
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    sleep 1
fi

info "Starting sidecar on :${PORT} using ${PYTHON}"

# ── Launch: prefer an isolated user service, else nohup ──────────────────────
if command -v systemd-run &>/dev/null && systemctl --user show-environment &>/dev/null; then
    systemd-run --user \
        --unit="$UNIT" \
        --collect \
        --description="UrbanIntegrity Scrapling planning sidecar" \
        -p "WorkingDirectory=${SIDECAR_DIR}" \
        -p "Environment=SIDECAR_PORT=${PORT}" \
        -p "StandardOutput=append:${LOG}" \
        -p "StandardError=append:${LOG}" \
        "$PYTHON" "${SIDECAR_DIR}/sidecar.py" \
        || die "systemd-run failed to launch $UNIT"
else
    warn "systemd-run --user unavailable — falling back to nohup."
    ( cd "$SIDECAR_DIR" && SIDECAR_PORT="$PORT" nohup "$PYTHON" sidecar.py >>"$LOG" 2>&1 & echo $! >"$PIDFILE" )
fi

# ── Wait for health ──────────────────────────────────────────────────────────
info "Waiting for sidecar to be ready..."
for i in $(seq 1 60); do
    if health; then
        ok "Sidecar ready (${i}×500ms) on http://127.0.0.1:${PORT}"
        exit 0
    fi
    sleep 0.5
done
echo "---- last lines of ${LOG} ----" >&2
tail -n 20 "$LOG" >&2 2>/dev/null || true
die "Sidecar did not become healthy within 30 s. See ${LOG}."
