#!/usr/bin/env bash
# scripts/toy-analysis.sh — provision, seed, project and probe the planted fixture analysis
# (spec §4.2). Idempotent: re-running re-seeds and re-projects the same analysis.
#
# Usage:   scripts/toy-analysis.sh
# Then:    urban-integrity-cli workbench --analysis <printed slug>
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LABEL="Planted fixture"
# Read the ward code from the fixture itself so this script cannot drift from the manifest.
WARD_CODE="$(sed -nE 's/^pub const WARD_CODE: &str = "([^"]+)";$/\1/p' crates/test-support/src/planted.rs)"
[[ -n "$WARD_CODE" ]] || die "could not read WARD_CODE from crates/test-support/src/planted.rs"
CLI=(cargo run -q -p urban-integrity-cli --)

curl -sf http://127.0.0.1:8000/health >/dev/null || die "SurrealDB not reachable on :8000 — run scripts/surreal-start.sh from the repo root"

WARDS_FILE="$(mktemp)"
trap 'rm -f "$WARDS_FILE"' EXIT
echo "$WARD_CODE" > "$WARDS_FILE"

# The db name is content-addressed: an_<slug(label)>_<sha256(wards)[..8]>. Ask the registry.
SLUG="$("${CLI[@]}" analysis list 2>/dev/null | awk -v l="$LABEL" '$0 ~ l {print $1; exit}' || true)"
if [[ -z "$SLUG" ]]; then
    info "creating analysis '$LABEL' (ward $WARD_CODE, --fixture)"
    "${CLI[@]}" analysis create --label "$LABEL" --wards-file "$WARDS_FILE" --fixture
    SLUG="$("${CLI[@]}" analysis list 2>/dev/null | awk -v l="$LABEL" '$0 ~ l {print $1; exit}')"
fi
[[ -n "$SLUG" ]] || die "could not find the fixture analysis in 'analysis list' output"
ok "analysis: $SLUG"

info "seeding planted fixture"
"${CLI[@]}" --analysis "$SLUG" fixture seed --planted --reseed

info "projecting"
"${CLI[@]}" --analysis "$SLUG" project

info "running the probe battery (Markdown to data/toy-evidence-report.md)"
mkdir -p data
"${CLI[@]}" --analysis "$SLUG" probe --output data/toy-evidence-report.md

ok "toy analysis ready"
echo "Next: cargo run -p urban-integrity-cli -- workbench --analysis $SLUG"
