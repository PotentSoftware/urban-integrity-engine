#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-fixture-not-shipped.sh — fail if the planted-graph test fixture
# assembler leaks into a shipped code path.
#
# `workbench_from_graph` (crates/workbench/src/graph/fixture.rs) is a public,
# `#[doc(hidden)]` assembler that exists ONLY so integration tests in `tests/`
# — which see the crate's public API and nothing else — can build a handle +
# `WorkbenchState` from a literal node/edge list. It fabricates a
# `PopulationSummary::Available` and an `AnalysisCatalogEntry` out of thin air
# and never touches SurrealDB, so a shipped caller would serve a made-up
# population as if it were a real analysis.
#
# Being `pub` is what makes that reachable, and `#[doc(hidden)]` is a docs
# convention, not a compiler barrier — nothing in rustc stops `boot.rs` or a
# handler from calling it. This script is that barrier: every mention of the
# symbol outside its own definition, the test trees, and prose must fail —
# except inside an in-crate `#[cfg(test)] mod tests { ... }` block, which only
# `cargo test` compiles (see only_in_test_module).
#
# Usage:   scripts/check-fixture-not-shipped.sh
#          scripts/check-fixture-not-shipped.sh --self-test
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

SYMBOL='workbench_from_graph'
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Paths permitted to name the symbol. Anchored at the repository root, matched
# as a path PREFIX (so a directory entry covers everything beneath it):
#   - the definition itself,
#   - the two test trees that drive it (workbench integration tests and the
#     CLI's own tests, which seed the planted fixture through the CLI),
#   - prose: docs/, .planning/ and the CI workflow comment, where the plans quote the signature,
#   - this script, which must name the symbol to grep for it.
ALLOWED=(
    'crates/workbench/src/graph/fixture.rs'
    'crates/workbench/tests/'
    'crates/cli/tests/'
    'docs/'
    '.planning/'
    '.github/workflows/ci.yml'
    'scripts/check-fixture-not-shipped.sh'
)

# is_allowed PATH
#
# Exit 0 iff PATH (repo-relative) sits under one of ALLOWED.
is_allowed() {
    local path="$1" allowed
    for allowed in "${ALLOWED[@]}"; do
        if [[ "$path" == "$allowed" || "$path" == "$allowed"* ]]; then
            return 0
        fi
    done
    return 1
}

# only_in_test_module FILE
#
# Exit 0 iff FILE is a Rust source whose every mention of the symbol sits inside
# a `#[cfg(test)]`-gated `mod tests { ... }` block. Such a block is compiled
# only under `cargo test`, so an in-crate unit test calling the assembler is
# test code even though its file is shipped. The block's extent is found by
# brace counting from its `mod tests {` line; a mention anywhere outside every
# such block (or in a non-Rust file) is still a violation.
only_in_test_module() {
    local file="$1"
    [[ "$file" == *.rs ]] || return 1
    awk -v sym="$SYMBOL" '
        BEGIN { gated = 0; depth = 0; outside = 0; seen = 0 }
        {
            line = $0
            if (depth == 0) {
                if (line ~ /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/) { gated = 1; next }
                if (gated && line ~ /^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+tests[[:space:]]*\{/) {
                    depth = gsub(/\{/, "{", line) - gsub(/\}/, "}", line)
                    gated = 0
                    if (index($0, sym)) seen = 1
                    next
                }
                if (line !~ /^[[:space:]]*(#\[|\/\/|$)/) gated = 0
                if (index($0, sym)) { seen = 1; outside = 1 }
                next
            }
            if (index($0, sym)) seen = 1
            depth += gsub(/\{/, "{", line) - gsub(/\}/, "}", line)
        }
        END { exit (seen && !outside) ? 0 : 1 }
    ' "$file"
}

# hits
#
# Every tracked file naming the symbol, one repo-relative path per line.
# `git grep -l` scopes the sweep to tracked files, so target/, data/ and other
# gitignored trees can never produce a hit.
hits() {
    (cd "$REPO_ROOT" && git grep -lF "$SYMBOL" -- . || true)
}

# self_test
#
# Non-vacuity proof: the allowlist must actually reject something. A path that
# is NOT under any allowed prefix has to be judged a violation, and each
# allowed prefix has to be judged clean — otherwise this guard could pass
# against a real leak.
self_test() {
    info "self-test: proving check-fixture-not-shipped.sh can fail on a synthetic violation..."
    local bad
    for bad in 'crates/workbench/src/boot.rs' \
               'crates/workbench/src/graph/handlers.rs' \
               'crates/cli/src/main.rs' \
               'crates/workbench/src/graph/fixture_helper.rs'; do
        if is_allowed "$bad"; then
            die "self-test FAILED — '$bad' is a shipped path but the allowlist accepts it (this guard could never catch a real leak)"
        fi
    done
    local good
    for good in 'crates/workbench/src/graph/fixture.rs' \
                'crates/workbench/tests/planted_golden.rs' \
                'crates/cli/tests/cli.rs' \
                'docs/superpowers/plans/x.md' \
                '.planning/STATE.md' \
                'scripts/check-fixture-not-shipped.sh'; do
        if ! is_allowed "$good"; then
            die "self-test FAILED — '$good' is an allowed path but the allowlist rejects it"
        fi
    done
    # The cfg(test)-module exemption must accept a mention inside the block and
    # reject one outside it, in the same file.
    local scratch
    scratch="$(mktemp -d)"
    printf 'fn shipped() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {\n        %s();\n    }\n}\n' "$SYMBOL" >"$scratch/in_tests.rs"
    printf 'fn shipped() {\n    %s();\n}\n\n#[cfg(test)]\nmod tests {\n    fn t() {\n        %s();\n    }\n}\n' "$SYMBOL" "$SYMBOL" >"$scratch/leaks.rs"
    printf '#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n\nfn shipped() {\n    %s();\n}\n' "$SYMBOL" >"$scratch/after_tests.rs"
    only_in_test_module "$scratch/in_tests.rs" \
        || die "self-test FAILED — a mention inside a #[cfg(test)] mod tests block was judged a leak"
    if only_in_test_module "$scratch/leaks.rs"; then
        die "self-test FAILED — a mention in shipped code was excused because the file also has a test module"
    fi
    if only_in_test_module "$scratch/after_tests.rs"; then
        die "self-test FAILED — a mention after the test module closed was excused"
    fi
    rm -rf "$scratch"

    # The symbol must still exist somewhere, or the real run below passes
    # vacuously because there is nothing left to guard.
    if [[ -z "$(hits)" ]]; then
        die "self-test FAILED — no tracked file names '$SYMBOL' at all (the guard has nothing to check)"
    fi
    ok "self-test passed — the allowlist rejects shipped paths, accepts the test/prose paths, and the symbol exists"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

violations=()
while IFS= read -r file; do
    [[ -z "$file" ]] && continue
    is_allowed "$file" && continue
    only_in_test_module "$REPO_ROOT/$file" || violations+=("$file")
done < <(hits)

if (( ${#violations[@]} > 0 )); then
    for file in "${violations[@]}"; do
        echo -e "${RED}[fail]${RESET}  $file names '$SYMBOL'" >&2
    done
    die "the test-only fixture assembler '$SYMBOL' is referenced outside crates/workbench/src/graph/fixture.rs, the test trees, and prose — it fabricates a population and must never enter a shipped code path"
fi

ok "'$SYMBOL' is confined to its definition, the test trees, and prose"
