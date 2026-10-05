#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# scripts/check.sh — one-shot local CI gate (mirrors .github/workflows/ci.yml).
#
# Runs EVERY gate even if an earlier one fails, then prints a summary and exits
# non-zero if any failed. This fixes the `fmt && clippy` foot-gun where a fmt
# failure short-circuits and clippy never runs.
#
# Usage:
#   scripts/check.sh              # fmt --check, clippy -D warnings, workspace tests
#   scripts/check.sh --fix        # run `cargo fmt --all` first, then the gates
#   scripts/check.sh --db         # also run the SurrealDB-gated suite (URBAN_DB_TESTS=1)
#   scripts/check.sh --fix --db
#
# Full logs: /tmp/urban-check-<timestamp>.log  (path printed at the end)
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

# Load cargo/rustup onto PATH (rust-toolchain.toml auto-selects the pinned 1.96.0).
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
LOG="/tmp/urban-check-$(date +%Y%m%d-%H%M%S).log"

FIX=false
RUN_DB=false
for arg in "$@"; do
  case "$arg" in
    --fix) FIX=true ;;
    --db)  RUN_DB=true ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo -e "${RED}Unknown arg: $arg${RESET} (try --fix, --db, --help)"; exit 2 ;;
  esac
done

command -v cargo >/dev/null || { echo -e "${RED}cargo not found — install Rust (rustup) or 'source \$HOME/.cargo/env'${RESET}"; exit 2; }

# Results accumulator: "name=STATUS"
declare -a RESULTS=()
OVERALL=0

# run <name> <command...> — runs a gate, tees to the log, records pass/fail.
run() {
  local name="$1"; shift
  echo -e "\n${CYAN}━━ ${name} ━━${RESET}" | tee -a "$LOG"
  if "$@" 2>&1 | tee -a "$LOG"; then
    RESULTS+=("$name=PASS")
    echo -e "${GREEN}[pass] ${name}${RESET}"
  else
    RESULTS+=("$name=FAIL")
    echo -e "${RED}[fail] ${name}${RESET}"
    OVERALL=1
  fi
}

echo "UrbanIntegrity local CI gate — $(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee "$LOG"

if $FIX; then
  echo -e "${YELLOW}--fix: applying cargo fmt --all${RESET}" | tee -a "$LOG"
  cargo fmt --all 2>&1 | tee -a "$LOG" || true
fi

run "fmt (check)"   cargo fmt --all --check
run "clippy (-D warnings)"  cargo clippy --workspace --all-targets -- -D warnings
run "test (workspace, no DB)"  cargo test --workspace

if $RUN_DB; then
  # Bring SurrealDB up (idempotent, isolated + memory-capped) if it isn't already.
  if ! curl -sf --max-time 3 http://localhost:8000/health >/dev/null 2>&1 \
     && ! curl -sf --max-time 3 "http://[::1]:8000/health" >/dev/null 2>&1; then
    echo -e "${YELLOW}SurrealDB not up — starting via surreal-start.sh${RESET}" | tee -a "$LOG"
    "$SCRIPT_DIR/surreal-start.sh" 2>&1 | tee -a "$LOG" || {
      echo -e "${RED}Could not start SurrealDB — skipping DB-gated suite${RESET}" | tee -a "$LOG"
      RESULTS+=("test (DB-gated)=SKIP")
    }
  fi
  if curl -sf --max-time 3 http://localhost:8000/health >/dev/null 2>&1 \
     || curl -sf --max-time 3 "http://[::1]:8000/health" >/dev/null 2>&1; then
    run "test (DB-gated, URBAN_DB_TESTS=1)"  env URBAN_DB_TESTS=1 cargo test --workspace
  fi
fi

echo -e "\n${CYAN}════════ summary ════════${RESET}"
for r in "${RESULTS[@]}"; do
  name="${r%=*}"; status="${r#*=}"
  case "$status" in
    PASS) echo -e "  ${GREEN}✓${RESET} $name" ;;
    FAIL) echo -e "  ${RED}✗${RESET} $name" ;;
    *)    echo -e "  ${YELLOW}–${RESET} $name ($status)" ;;
  esac
done
echo -e "\nFull log: ${LOG}"
if [[ $OVERALL -eq 0 ]]; then
  echo -e "${GREEN}ALL GATES PASSED${RESET}"
else
  echo -e "${RED}ONE OR MORE GATES FAILED${RESET} — see the log above."
fi
exit $OVERALL
