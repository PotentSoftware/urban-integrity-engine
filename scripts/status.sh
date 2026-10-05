#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# scripts/status.sh — "where are we?" snapshot to regain context in one command.
#
# Prints: recent commits, uncommitted-change summary, OPEN debug findings, specs
# awaiting a build, and the tail of the durable worklog (.planning/WORKLOG.md).
# Read-only — never changes anything.
#
# Usage: scripts/status.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || { cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd; })"
cd "$ROOT"

CYAN='\033[0;36m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; RESET='\033[0m'
hdr() { echo -e "\n${CYAN}━━ $* ━━${RESET}"; }

hdr "Recent commits"
git log --oneline -8 2>/dev/null || echo "  (no git history)"

hdr "Uncommitted changes"
staged=$(git diff --cached --name-only | wc -l | tr -d ' ')
unstaged=$(git diff --name-only | wc -l | tr -d ' ')
untracked=$(git ls-files --others --exclude-standard | wc -l | tr -d ' ')
echo "  staged: $staged   unstaged: $unstaged   untracked: $untracked"
if [[ "$staged$unstaged$untracked" != "000" ]]; then
  git status --short | sed 's/^/  /' | head -30
fi

hdr "Open debug findings (.planning/debug)"
found=false
for f in .planning/debug/*.md; do
  [[ -e "$f" ]] || continue
  if grep -qi '^status: open' "$f"; then
    found=true
    trig=$(grep -i '^trigger:' "$f" | head -1 | sed 's/^[Tt]rigger:[[:space:]]*//')
    echo -e "  ${YELLOW}●${RESET} $(basename "$f")"
    echo "      ${trig:0:140}"
  fi
done
$found || echo -e "  ${GREEN}none open${RESET}"

hdr "Specs awaiting a build (.planning/specs)"
ls -1 .planning/specs/*.md 2>/dev/null | sed 's|.*/|  |' || echo "  (none)"

hdr "Worklog (tail)"
if [[ -f .planning/WORKLOG.md ]]; then
  tail -n 30 .planning/WORKLOG.md | sed 's/^/  /'
else
  echo "  (no .planning/WORKLOG.md yet)"
fi
echo
