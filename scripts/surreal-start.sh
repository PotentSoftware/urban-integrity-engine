#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# surreal-start.sh — launch SurrealDB ISOLATED from the calling shell.
#
# Why: the CCOD ingest can drive surrealkv to >14 GB RSS. When surreal is
# backgrounded from a VS Code integrated terminal it lives in VS Code's cgroup
# (app-code-*.scope), so a DB OOM takes the whole editor down with it
# (kernel global OOM, 2026-06-17 00:20 & 10:01).
#
# Two launchers, picked by URBAN_SURREAL_LAUNCHER (default: auto):
#
#   systemd     A transient *user systemd service* under user.slice — detached
#               from any terminal — with a hard cgroup memory cap. If surreal
#               exceeds the cap the cgroup OOM killer reaps surreal ALONE; the
#               editor and the rest of the host survive. Preferred.
#   background  For hosts with no systemd user session (macOS, WSL without
#               systemd, containers). A detached background process with a PID
#               file. It CANNOT hard-cap memory — an address-space ulimit breaks
#               RocksDB's mmap'd I/O — so a large ingest can still exhaust RAM.
#   auto        systemd when `systemd-run` exists and a user manager answers,
#               otherwise background.
#
# Both launch from the repository root, so the relative store path
# (rocksdb://data/surreal-rocks.db) always resolves to this checkout's data/.
# Idempotent: re-running is a no-op if a healthy instance is already up.
#
# Usage:   scripts/surreal-start.sh
# Stop:    scripts/surreal-start.sh --stop     (either launcher)
# Logs:    data/surreal.log   (systemd: also journalctl --user -u urban-surreal -f)
# Tune:    URBAN_SURREAL_MEM_MAX=8G scripts/surreal-start.sh     (systemd only)
# Force:   URBAN_SURREAL_LAUNCHER=background scripts/surreal-start.sh
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SURREAL_BIN="${SURREAL_BIN:-${HOME}/.surrealdb/surreal}"
# RocksDB backend (LSM: bounded memtables + block cache, evicts to SST on disk).
# Replaced surrealkv:// on 2026-06-17 — surrealkv held every touched key resident
# in RAM and OOM-killed surreal during the CCOD ingest.
DB_PATH="${URBAN_SURREAL_DB_PATH:-rocksdb://data/surreal-rocks.db}"
SURREAL_BIND="${URBAN_SURREAL_BIND:-[::]:8000}"
# Port the health check probes — the part of the bind address after the last ':'.
SURREAL_PORT="${SURREAL_BIND##*:}"
LAUNCHER="${URBAN_SURREAL_LAUNCHER:-auto}"
UNIT="urban-surreal"
# Hard cap (cgroup memory.max). Host is 32 GB; 12 G leaves ~20 G for editor+OS.
MEM_MAX="${URBAN_SURREAL_MEM_MAX:-12G}"
# Soft cap (memory.high): kernel throttles + reclaims here, nudging the storage
# engine to flush memtables before the hard wall.
MEM_HIGH="${URBAN_SURREAL_MEM_HIGH:-9G}"
LOG_FILE="${WORKSPACE_ROOT}/data/surreal.log"
PID_FILE="${WORKSPACE_ROOT}/data/surreal.pid"
# On-disk store dir (DB_PATH minus the scheme), used for stale-lock cleanup.
# Empty for schemeless backends like `memory`.
STORE_DIR=""
case "$DB_PATH" in
    *://*) STORE_DIR="${WORKSPACE_ROOT}/${DB_PATH#*://}" ;;
esac

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

health() { curl -sf "http://127.0.0.1:${SURREAL_PORT}/health" &>/dev/null \
        || curl -sf "http://[::1]:${SURREAL_PORT}/health" &>/dev/null; }

# True when a systemd user manager is reachable — `systemd-run` alone is not
# enough (WSL and containers often ship the binary with no user session).
systemd_user_available() {
    command -v systemd-run &>/dev/null \
        && systemctl --user show-environment &>/dev/null
}

# PIDs of surreal servers started from SURREAL_BIN and bound to SURREAL_BIND.
# Matching the bind address too means a second instance on another port (or a
# test instance) is never mistaken for this one and killed. Uses `ps` rather
# than /proc so it works on macOS.
our_server_pids() {
    local pid args
    for pid in $(pgrep -f -- "${SURREAL_BIN} start" 2>/dev/null || true); do
        args="$(ps -o args= -p "$pid" 2>/dev/null || true)"
        [[ "$args" == "${SURREAL_BIN} start"* && "$args" == *"--bind ${SURREAL_BIND} "* ]] \
            && echo "$pid"
    done
    return 0
}

pid_alive() { [[ -n "${1:-}" ]] && kill -0 "$1" 2>/dev/null; }

background_pid() {
    [[ -f "$PID_FILE" ]] || return 0
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    if pid_alive "$pid"; then echo "$pid"; fi
    return 0
}

# Stops this checkout's server, whichever launcher started it, and waits for exit.
stop_all() {
    local stopped=0 pid
    if command -v systemctl &>/dev/null \
        && systemctl --user --quiet is-active "$UNIT" 2>/dev/null; then
        info "Stopping systemd unit $UNIT..."
        systemctl --user stop "$UNIT" 2>/dev/null || true
        stopped=1
    fi
    for pid in $( { background_pid; our_server_pids; } | sort -u ); do
        pid_alive "$pid" || continue
        info "Stopping surreal process $pid..."
        kill "$pid" 2>/dev/null || true
        stopped=1
    done
    for _ in $(seq 1 20); do
        [[ -z "$(our_server_pids)" ]] && break
        sleep 0.5
    done
    if [[ -n "$(our_server_pids)" ]]; then
        warn "surreal still running after 10 s — sending SIGKILL"
        for pid in $(our_server_pids); do kill -9 "$pid" 2>/dev/null || true; done
        sleep 1
    fi
    rm -f "$PID_FILE"
    return $(( stopped == 0 ))
}

if [[ "${1:-}" == "--stop" ]]; then
    if stop_all; then ok "SurrealDB stopped."; else info "No SurrealDB instance from this checkout was running."; fi
    exit 0
elif [[ -n "${1:-}" ]]; then
    die "unknown argument '$1' (usage: scripts/surreal-start.sh [--stop])"
fi

[[ -x "$SURREAL_BIN" ]] || die "surreal binary not found at $SURREAL_BIN (set SURREAL_BIN to override)"

case "$LAUNCHER" in
    auto)
        if systemd_user_available; then LAUNCHER=systemd; else LAUNCHER=background; fi ;;
    systemd)
        systemd_user_available \
            || die "URBAN_SURREAL_LAUNCHER=systemd but no systemd user session is reachable (try URBAN_SURREAL_LAUNCHER=background)" ;;
    background) ;;
    *) die "URBAN_SURREAL_LAUNCHER must be auto, systemd or background (got '$LAUNCHER')" ;;
esac

# ── Idempotency: healthy instance already up? ────────────────────────────────
if health; then
    if [[ "$LAUNCHER" == systemd ]] && systemctl --user --quiet is-active "$UNIT" 2>/dev/null; then
        ok "SurrealDB already running (unit: $UNIT) and healthy — nothing to do."
        systemctl --user show "$UNIT" -p MemoryMax -p MemoryCurrent 2>/dev/null \
            | sed 's/^/       /'
        exit 0
    fi
    if [[ "$LAUNCHER" == background && -n "$(background_pid)" ]]; then
        ok "SurrealDB already running (pid $(background_pid), background launcher) and healthy — nothing to do."
        exit 0
    fi
fi

# ── Tear down anything stale ─────────────────────────────────────────────────
if command -v systemctl &>/dev/null; then
    systemctl --user reset-failed "$UNIT" 2>/dev/null || true
fi
if stop_all; then
    warn "Stopped a stale or differently-launched surreal instance — waiting for port..."
    sleep 1
fi
if [[ -n "$STORE_DIR" && -f "${STORE_DIR}/LOCK" ]]; then
    warn "Removing stale storage LOCK at ${STORE_DIR}/LOCK"
    rm -f "${STORE_DIR}/LOCK"
fi

mkdir -p "${WORKSPACE_ROOT}/data"

# ── Launch ───────────────────────────────────────────────────────────────────
if [[ "$LAUNCHER" == systemd ]]; then
    info "Starting SurrealDB as user service '$UNIT' (MemoryMax=$MEM_MAX, MemoryHigh=$MEM_HIGH)..."
    systemd-run --user \
        --unit="$UNIT" \
        --collect \
        --description="UrbanIntegrity SurrealDB (memory-capped, isolated from editor)" \
        -p "WorkingDirectory=${WORKSPACE_ROOT}" \
        -p "MemoryMax=${MEM_MAX}" \
        -p "MemoryHigh=${MEM_HIGH}" \
        -p "MemorySwapMax=2G" \
        -p "OOMPolicy=stop" \
        -p "StandardOutput=append:${LOG_FILE}" \
        -p "StandardError=append:${LOG_FILE}" \
        "$SURREAL_BIN" start \
            --user root --pass root \
            --bind "$SURREAL_BIND" \
            "$DB_PATH" \
        || die "systemd-run failed to launch $UNIT"
else
    warn "Background launcher (no systemd user session, or URBAN_SURREAL_LAUNCHER=background) — starting a detached process."
    warn "This launcher has NO memory cap: a large ingest (e.g. HMLR CCOD) can exhaust RAM."
    # A subshell that cd's to the repo root so the relative store path resolves to
    # this checkout's data/, then detaches: setsid (Linux) puts surreal in its own
    # session so closing the terminal does not SIGHUP it; nohup covers macOS.
    (
        cd "$WORKSPACE_ROOT"
        if command -v setsid &>/dev/null; then
            setsid nohup "$SURREAL_BIN" start \
                --user root --pass root \
                --bind "$SURREAL_BIND" \
                "$DB_PATH" >>"$LOG_FILE" 2>&1 < /dev/null &
        else
            nohup "$SURREAL_BIN" start \
                --user root --pass root \
                --bind "$SURREAL_BIND" \
                "$DB_PATH" >>"$LOG_FILE" 2>&1 < /dev/null &
        fi
        echo $! >"$PID_FILE"
    )
fi

still_running() {
    if [[ "$LAUNCHER" == systemd ]]; then
        systemctl --user --quiet is-active "$UNIT" 2>/dev/null
    else
        [[ -n "$(background_pid)" ]]
    fi
}

# ── Wait for health ──────────────────────────────────────────────────────────
info "Waiting for SurrealDB to be ready..."
for i in $(seq 1 60); do
    if health; then
        if [[ "$LAUNCHER" == systemd ]]; then
            ok "SurrealDB ready (${i}×500ms) — isolated in user.slice, capped at $MEM_MAX."
            info "Watch RSS:  systemctl --user show $UNIT -p MemoryCurrent"
        else
            ok "SurrealDB ready (${i}×500ms) — background pid $(background_pid), no memory cap."
        fi
        info "Stop with:  scripts/surreal-start.sh --stop"
        exit 0
    fi
    if ! still_running; then
        echo "---- last lines of data/surreal.log ----" >&2
        tail -n 20 "$LOG_FILE" >&2 2>/dev/null || true
        rm -f "$PID_FILE"
        die "SurrealDB exited during startup (OOM or crash). See data/surreal.log."
    fi
    sleep 0.5
done
die "SurrealDB did not become healthy within 30 s. Check data/surreal.log."
