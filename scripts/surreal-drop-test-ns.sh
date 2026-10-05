#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# surreal-drop-test-ns.sh — sweep up leaked `test*` namespaces.
#
# Every DB-gated test runs in a throwaway namespace owned by an `NsGuard`
# (crates/test-support), which removes it on drop. Drop does not run when a
# test binary is SIGKILLed or aborts, so a crashed run can still strand one.
# This script is the backstop for that case.
#
# It only ever touches namespaces whose name starts with `test`, which is the
# prefix `NsGuard::with_prefix` enforces. Production namespaces
# (urban_integrity, urban, main) can never match.
#
# Usage:   scripts/surreal-drop-test-ns.sh          # list what would go
#          scripts/surreal-drop-test-ns.sh --apply  # actually remove them
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

ENDPOINT="${URBAN_TEST_DB_URL:-http://127.0.0.1:8000}"
USER="${URBAN__SURREAL__USERNAME:-root}"
PASS="${URBAN__SURREAL__PASSWORD:-root}"
BATCH=50

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

APPLY=0
[[ "${1:-}" == "--apply" ]] && APPLY=1

sql() { curl -sf -m 120 -X POST "${ENDPOINT}/sql" -u "${USER}:${PASS}" \
             -H "Accept: application/json" --data-binary "$1"; }

sql "INFO FOR ROOT;" >/dev/null 2>&1 || die "SurrealDB unreachable at ${ENDPOINT}"

mapfile -t LEAKED < <(sql "INFO FOR ROOT;" | python3 -c "
import json, sys
ns = json.load(sys.stdin)[0]['result']['namespaces']
print('\n'.join(n for n in sorted(ns) if n.startswith('test')))
")

if [[ ${#LEAKED[@]} -eq 0 || -z "${LEAKED[0]}" ]]; then
    ok "No leaked test namespaces."
    exit 0
fi

info "Found ${#LEAKED[@]} leaked test namespace(s)."
if [[ $APPLY -eq 0 ]]; then
    printf '       %s\n' "${LEAKED[@]:0:20}"
    [[ ${#LEAKED[@]} -gt 20 ]] && echo "       … and $(( ${#LEAKED[@]} - 20 )) more"
    warn "Dry run — re-run with --apply to remove them."
    exit 0
fi

removed=0
total=${#LEAKED[@]}
for (( i=0; i<total; i+=BATCH )); do
    chunk=( "${LEAKED[@]:i:BATCH}" )
    stmt=""
    for ns in "${chunk[@]}"; do
        stmt+="REMOVE NAMESPACE IF EXISTS \`${ns}\`;"
    done
    sql "$stmt" >/dev/null || die "batch starting at index $i failed"
    removed=$(( removed + ${#chunk[@]} ))
    info "removed ${removed}/${total}"
done

ok "Removed ${#LEAKED[@]} leaked test namespace(s)."
