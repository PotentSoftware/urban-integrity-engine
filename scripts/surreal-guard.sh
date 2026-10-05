#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# surreal-guard.sh — interactive-shell guard that stops a bare `surreal start`
# from bypassing the project's capped, isolated launcher.
#
# Why this exists (debug: surreal-oom-recurrence, 2026-06-17):
#   `surreal` was OOM-killed 3× in one day. Twice it had been launched the OLD
#   way — a bare `surreal start … surrealkv://data/surreal.db` reused from shell
#   history — which reintroduces BOTH defects the 2026-06-17 RocksDB fix removed:
#     1. surrealkv storage engine (unbounded resident key index → the OOM cause).
#     2. No cgroup memory cap + wrong cgroup: a terminal launch inherits the
#        caller's cgroup (VS Code's app-code-*.scope), so a DB OOM takes the
#        whole editor down with it (kernel GLOBAL OOM).
#   The correct path — scripts/surreal-start.sh → urban-surreal user service,
#   RocksDB, MemoryMax=12G/MemoryHigh=9G under user.slice — was unused.
#
# This guard wraps the `surreal` command. It intercepts ONLY the `start`
# subcommand, warns, and routes to scripts/surreal-start.sh. Every other
# subcommand (sql, import, export, version, …) passes through untouched.
#
# Install (opt-in): add `source /path/to/urban-integrity/scripts/surreal-guard.sh`
# to ~/.bashrc. It is safe to source repeatedly — the function is simply
# (re)defined. Only useful where scripts/surreal-start.sh works (systemd user
# session); do not install it on a host that has to launch surreal by hand.
#
# Override (rare, you really mean it): set SURREAL_GUARD_BYPASS=1 for one call:
#   SURREAL_GUARD_BYPASS=1 surreal start …
# ─────────────────────────────────────────────────────────────────────────────

# Path to the canonical launcher. Defaults to the checkout this file was sourced
# from (resolved once, at source time); override with URBAN_INTEGRITY_DIR.
__URBAN_GUARD_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)"
__urban_surreal_launcher() {
    local repo="${URBAN_INTEGRITY_DIR:-$__URBAN_GUARD_REPO}"
    printf '%s/scripts/surreal-start.sh' "$repo"
}

surreal() {
    # Resolve the real binary so non-start subcommands pass straight through.
    local real_surreal
    real_surreal="$(command -v -- surreal 2>/dev/null)"
    [[ -z "$real_surreal" || "$real_surreal" == "surreal" ]] \
        && real_surreal="$HOME/.surrealdb/surreal"

    # Find the first non-flag argument — that's the subcommand.
    local sub=""
    local a
    for a in "$@"; do
        case "$a" in
            -*) continue ;;   # skip global flags like -v / --help
            *)  sub="$a"; break ;;
        esac
    done

    if [[ "$sub" == "start" && -z "${SURREAL_GUARD_BYPASS:-}" ]]; then
        local launcher; launcher="$(__urban_surreal_launcher)"
        echo "[surreal-guard] Refusing a bare 'surreal start'." >&2
        echo "[surreal-guard] A direct launch runs surreal UNCAPPED in this shell's cgroup" >&2
        echo "[surreal-guard] (e.g. VS Code's app-code-*.scope) and can global-OOM the editor." >&2
        echo "[surreal-guard] See debug: surreal-oom-recurrence (2026-06-17)." >&2
        if [[ -x "$launcher" ]]; then
            echo "[surreal-guard] Routing to capped/isolated launcher: $launcher" >&2
            "$launcher"
            return $?
        fi
        echo "[surreal-guard] Launcher not found/executable at: $launcher" >&2
        echo "[surreal-guard] Run scripts/surreal-start.sh from the urban-integrity repo," >&2
        echo "[surreal-guard] or set URBAN_INTEGRITY_DIR. To truly bypass: SURREAL_GUARD_BYPASS=1 surreal start …" >&2
        return 1
    fi

    # Anything that is not `start` (sql, import, export, version, …) runs as-is.
    "$real_surreal" "$@"
}
