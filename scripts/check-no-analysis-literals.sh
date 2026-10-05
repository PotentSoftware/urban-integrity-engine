#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-no-analysis-literals.sh — fail if an Anglesey-specific literal appears
# in workbench source (D-10 amended).
#
# The workbench app is analysis-scoped from day one: no Anglesey-specific
# strings, ward codes or counts anywhere in the UI or its fixtures. This
# phase's analysis picker (D-10, amended 2026-09-04 at plan check) shows the
# exact restart command for picking another analysis rather than live
# re-targeting — so there is no legitimate reason for an analysis-specific
# literal to appear in source at all, this phase or any later one.
#
# Case-insensitive so "Anglesey"/"ANGLESEY"/"anglesey" are all caught, and so
# a differently-cased ward-code prefix does not slip past.
#
# Usage:   scripts/check-no-analysis-literals.sh          # checks the real workspace
#          scripts/check-no-analysis-literals.sh --self-test
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# `anglesey`, the analysis-id suffix `aebaa785`, and any W05-prefixed ONS ward
# code (six digits after the W05 nation+category prefix, e.g. W05001492).
#
# Fixtures that need a ward-code-shaped string use the deliberately synthetic
# `X05…` prefix instead (`X05001499`, `X05009901`): `X` is not an ONS
# nation+category prefix, so such a code can never name a real ward, while
# still satisfying `naming::validate_ward_code`'s shape rule (one uppercase
# letter + eight digits). Do not "correct" a fixture back to `W05…` — that is
# the exact drift this guard exists to catch, and it went unnoticed across
# plans 44-02 to 44-21 until the phase-44 QA gate.
FORBIDDEN='anglesey|aebaa785|W05[0-9]{6}'

# check_tree TREE
#
# Fails if any file under TREE contains a FORBIDDEN literal (case-insensitive).
# A tree that does not exist yet is skipped with a warning, not a failure —
# the workbench crates land across several plans in this phase, and this
# script must be safe to wire into CI from this wave onward regardless.
#
# Scanned per crate ROOT (not just `src/`): the DTO crate's fixtures are the
# wire contract the UI consumes, `index.html`/`style/`/`js-build/` are UI
# source, and `Cargo.toml`/`Trunk.toml` are configuration — D-10 (amended)
# forbids an analysis-specific literal "anywhere in the UI or its fixtures".
# The original `crates/workbench-web/src` + `crates/workbench/src` scope left
# `crates/workbench-dto` shipping live analysis identifiers and corpus counts
# in its fixtures, and made the html/css/js include globs below dead code
# (Phase 42 code review, WR-07). Build outputs and vendored/installed trees
# are excluded — `dist/` and `node_modules/` are not source, and `target/` is
# never inside a crate root anyway.
check_tree() {
    local tree="$1"
    if [[ ! -d "$tree" ]]; then
        warn "$tree does not exist yet — nothing to check"
        return 0
    fi
    local hit
    hit=$(grep -rniE "$FORBIDDEN" "$tree" --include="*.rs" --include="*.html" \
               --include="*.css" --include="*.js" --include="*.toml" \
               --exclude-dir=dist --exclude-dir=node_modules --exclude-dir=target \
               2>/dev/null || true)
    if [[ -n "$hit" ]]; then
        echo "$hit" >&2
        die "$tree: an Anglesey-specific literal was found (see hits above) — the workbench must stay analysis-scoped (D-10 amended)"
    fi
    ok "$tree: no Anglesey-specific literal found"
}

# Hidden subcommand: lets --self-test run check_tree as a SEPARATE process
# (subprocess re-invocation of this same script) so it can assert on that
# process's own exit code without the self-test's own `set -e` treating the
# expected `die()` as aborting the whole self-test.
if [[ "${1:-}" == "__check_tree" ]]; then
    check_tree "$2"
    exit 0
fi

self_test() {
    info "self-test: proving check-no-analysis-literals.sh can fail on a synthetic violation..."
    local scratch_dir
    # mktemp -d always resolves outside the repository's git working tree —
    # stronger than "a gitignored scratch path within one of the two scanned
    # trees" (crates/workbench-web/src and crates/workbench/src do not exist
    # in every worktree checkout this phase — see check_tree's doc comment —
    # so writing directly into either during a self-test risks leaving stray
    # directories behind if the run is interrupted). This directory is never
    # part of the working tree, is scanned via the exact same check_tree
    # function the real run uses, and is fully removed before returning.
    scratch_dir="$(mktemp -d)"
    mkdir -p "${scratch_dir}/src"
    echo 'const ANALYSIS_ID: &str = "an_anglesey_aebaa785";' > "${scratch_dir}/src/fixture.rs"

    local self_test_out
    self_test_out="$(mktemp)"
    if "${BASH_SOURCE[0]}" __check_tree "$scratch_dir" >"$self_test_out" 2>&1; then
        cat "$self_test_out" >&2
        rm -rf "$scratch_dir" "$self_test_out"
        die "self-test FAILED — the guard did NOT flag a synthetic 'aebaa785' literal (non-vacuity broken)"
    fi
    rm -rf "$scratch_dir" "$self_test_out"
    ok "self-test passed — the guard correctly flagged the synthetic violation"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

# Every workbench-facing crate ROOT (workbench, workbench-dto, workbench-web,
# and any later sibling) — matching check-verdict-deserialize.sh's
# `crates/workbench*` glob, but at the crate root rather than `src/` so
# fixtures, html/css/js and manifests are in scope too.
shopt -s nullglob
WORKBENCH_ROOTS=()
for dir in "${REPO_ROOT}"/crates/workbench*/; do
    WORKBENCH_ROOTS+=("${dir%/}")
done
shopt -u nullglob

if [[ ${#WORKBENCH_ROOTS[@]} -eq 0 ]]; then
    warn "no crates/workbench* crates exist yet — nothing to check"
else
    for root in "${WORKBENCH_ROOTS[@]}"; do
        check_tree "$root"
    done
fi
