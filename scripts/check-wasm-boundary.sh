#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-wasm-boundary.sh — fail if a forbidden crate enters the wasm32
# dependency graph of a workbench-facing crate (WBX-03).
#
# The browser only ever sees the wasm-safe DTO crate and the frontend crate
# (D-01/D-02). A future contributor adding `urban-integrity-schema` (or one of
# its heavy siblings) "just to reuse a type" would silently reintroduce
# surrealdb/tokio/petgraph/rayon/graphrs/arrow into the wasm build graph — this
# is that mistake's mechanical guard, checked crate-by-crate so a failure
# names WHICH crate pulled the forbidden dependency in. `graphrs`/`arrow`
# (plan 44-15, Graph page community detection + Arrow IPC export) are native-
# only, added to `crates/workbench/Cargo.toml` alone — never to
# `crates/workbench-web/Cargo.toml`.
#
# `crates/schema` is forbidden OUTRIGHT (D-02's amendment), not merely its
# transitive `surrealdb` dev-dependency — a schema-only addition today is
# wasm-safe in isolation, but the crate itself is the thing to keep out, since
# any future `[dependencies]` addition to schema would break the wasm build
# silently otherwise. `-e normal` excludes dev/build dependency edges, which
# matters here because schema's own `surrealdb` dependency is currently a
# `[dev-dependencies]` entry that would not show up under `-e normal` at all —
# the crate-NAME check below catches schema regardless of which edge kind
# pulled it in.
#
# Usage:   scripts/check-wasm-boundary.sh [crate...]   # checks urban-integrity-workbench-dto
#                                                       # plus any extra crate names given
#          scripts/check-wasm-boundary.sh --self-test   # proves the guard can fail
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

# Anchored to the crate TOKEN, never to the line start. Every dependency line
# `cargo tree` prints is preceded by tree glyphs and indentation ("├── tokio
# v1.52.2", "│   └── petgraph v0.6.5") — only the root package starts at column
# 0 — so a `^tokio ` style anchor can never match a dependency at all (the
# original form of this pattern silently guarded nothing for the four
# non-`urban-integrity-*` crates; caught at Phase 42 code review, CR-01).
# `(^|[^a-z0-9_-])` requires the crate name to start after a non-name character
# (a glyph, a space, or the line start), and ` v[0-9]` requires it to be
# followed by a version — together these reject lookalikes such as
# "tokio-macros v2", "tokio-util v0.7" or "surrealdb-core v3" while still
# matching the bare crate. `tree_has_forbidden` below is the ONE place this
# pattern is applied, and --self-test proves it against synthetic tree lines
# for every crate named here (both the glyph-prefixed dependency form and the
# lookalike-rejection form) before touching cargo at all.
FORBIDDEN='(^|[^a-z0-9_-])(urban-integrity-schema|urban-integrity-core|urban-integrity-detector|surrealdb|tokio|petgraph|rayon|graphrs|arrow) v[0-9]'

# tree_has_forbidden TREE_TEXT
#
# Exit 0 iff TREE_TEXT (a `cargo tree` listing) names a forbidden crate. Shared
# by check_pkg (real run) and the self-test's synthetic-input proof so the two
# can never drift apart.
tree_has_forbidden() {
    echo "$1" | grep -qE "$FORBIDDEN"
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# check_pkg PKG [MANIFEST_PATH]
#
# Runs the exact wasm32 dependency-graph check the real (no-args) invocation
# uses. MANIFEST_PATH is only ever set by __check_pkg's self-test callback,
# which points this same function at a scratch, non-workspace crate — the
# self-test reuses this function verbatim rather than re-deriving the check.
check_pkg() {
    local pkg="$1"
    local manifest_path="${2:-}"
    local tree
    if [[ -n "$manifest_path" ]]; then
        tree=$(cargo tree --target wasm32-unknown-unknown -e normal --offline --manifest-path "$manifest_path" 2>&1) \
            || die "cargo tree failed to run for '$pkg' (manifest: $manifest_path): $tree"
    else
        # `-p` on a crate not (yet) in [workspace] members fails with "package ID
        # specification ... did not match any packages" — treated as "nothing to
        # check" rather than a hard failure. Several Phase 42 plans land the
        # workbench crates in parallel worktrees that merge later; a plan whose
        # own worktree hasn't seen a sibling plan's new crate yet must not fail
        # here just because that crate doesn't exist on THIS branch yet.
        local tree_out
        tree_out="$(mktemp)"
        if ! (cd "$REPO_ROOT" && cargo tree --target wasm32-unknown-unknown -p "$pkg" -e normal >"$tree_out" 2>&1); then
            tree="$(cat "$tree_out")"
            rm -f "$tree_out"
            if echo "$tree" | grep -qE "did not match any packages|no matching package"; then
                warn "$pkg: not present in this workspace checkout yet — nothing to check (will be caught once merged)"
                return 0
            fi
            die "cargo tree failed to run for workspace crate '$pkg': $tree"
        fi
        tree="$(cat "$tree_out")"
        rm -f "$tree_out"
    fi
    if tree_has_forbidden "$tree"; then
        local hit
        hit=$(echo "$tree" | grep -E "$FORBIDDEN" | head -1 | sed -E 's/^[^a-z]+//')
        die "'$pkg's wasm32 dependency graph contains a forbidden crate: $hit"
    fi
    ok "$pkg: no forbidden crate in the wasm32-unknown-unknown dependency graph"
}

# Hidden subcommand: lets --self-test run check_pkg as a SEPARATE process
# (via a subprocess re-invocation of this same script) so it can assert on
# that process's own exit code without the self-test's own `set -e` aborting
# the whole self-test on the expected failure.
if [[ "${1:-}" == "__check_pkg" ]]; then
    check_pkg "$2" "${3:-}"
    exit 0
fi

# self_test_pattern
#
# Pure-text, cargo-free proof that FORBIDDEN matches every forbidden crate in
# the shape `cargo tree` actually prints (glyph-prefixed, indented dependency
# lines — NOT column-0 lines) and rejects the lookalike crates that share a
# prefix. This is the check that would have caught the `^tokio ` anchoring bug:
# the four non-`urban-integrity-*` crates were unguarded because their patterns
# could only ever match a root package, and the scratch-crate self-test only
# ever planted one of the three unanchored names.
self_test_pattern() {
    local crate
    for crate in urban-integrity-schema urban-integrity-core urban-integrity-detector \
                 surrealdb tokio petgraph rayon graphrs arrow; do
        # Three real `cargo tree` dependency shapes: first-level, nested under
        # a continuation bar, and last-child at depth 2.
        local shapes
        shapes=$(printf '├── %s v1.2.3\n│   └── %s v0.6.5\n    └── %s v3.1.0-beta.2\n' \
                        "$crate" "$crate" "$crate")
        local line
        while IFS= read -r line; do
            if ! tree_has_forbidden "$line"; then
                die "self-test FAILED — FORBIDDEN does not match a real cargo-tree dependency line for '$crate': '$line' (non-vacuity broken: this guard could never catch a real violation)"
            fi
        done <<< "$shapes"
    done
    local lookalikes
    lookalikes=$(printf '├── tokio-macros v2.5.0\n│   └── tokio-util v0.7.13\n└── surrealdb-core v3.1.0-beta.2\n    └── petgraph-derive v0.1.0\n    └── rayon-core v1.12.1\n')
    if tree_has_forbidden "$lookalikes"; then
        die "self-test FAILED — FORBIDDEN matched a lookalike crate name (tokio-macros/tokio-util/surrealdb-core/rayon-core) that is NOT forbidden"
    fi
    ok "self-test: FORBIDDEN matches all 9 forbidden crates on glyph-prefixed dependency lines and rejects lookalikes"
}

# self_test_scratch_crate NAME DEP_LINE
#
# End-to-end proof through the real `cargo tree` invocation: a scratch,
# non-workspace crate whose only dependency is DEP_LINE must make check_pkg
# fail. mktemp -d always resolves outside the repository's git working tree —
# stronger than "gitignored", it is never part of the working tree at all.
self_test_scratch_crate() {
    local name="$1"
    local dep_line="$2"
    local scratch_dir
    scratch_dir="$(mktemp -d)"
    mkdir -p "${scratch_dir}/${name}/src"
    cat > "${scratch_dir}/${name}/Cargo.toml" <<EOF
[package]
name = "${name}"
version = "0.0.0"
edition = "2021"

[dependencies]
${dep_line}
EOF
    echo 'pub fn f() {}' > "${scratch_dir}/${name}/src/lib.rs"

    local self_test_out
    self_test_out="$(mktemp)"
    if "${BASH_SOURCE[0]}" __check_pkg "$name" \
        "${scratch_dir}/${name}/Cargo.toml" \
        > "$self_test_out" 2>&1; then
        cat "$self_test_out" >&2
        rm -rf "$scratch_dir" "$self_test_out"
        die "self-test FAILED — the guard did NOT fail against a scratch crate depending on '${dep_line}' (non-vacuity broken: this guard could never catch a real violation)"
    fi
    rm -rf "$scratch_dir" "$self_test_out"
    ok "self-test: the guard correctly failed against the synthetic dependency '${dep_line}'"
}

self_test() {
    info "self-test: proving check-wasm-boundary.sh can fail on a synthetic violation..."
    self_test_pattern
    # One crate from each half of the pattern: a path-dependency workspace
    # crate (urban-integrity-schema) AND a plain registry crate that was
    # formerly `^`-anchored (petgraph — pinned to the version already in the
    # workspace's Cargo.lock so `--offline` resolution never needs the network).
    self_test_scratch_crate scratch-wasm-check-schema \
        "urban-integrity-schema = { path = \"${REPO_ROOT}/crates/schema\" }"
    self_test_scratch_crate scratch-wasm-check-petgraph \
        'petgraph = "=0.6.5"'
    ok "self-test passed — the guard fails on every synthetic violation it was given"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

# Real run: default checked-crate list (workbench-dto) plus any extra crate
# names given as positional args, so later plans can extend the list without
# editing this script (e.g. once urban-integrity-workbench-web exists).
PKGS_RAW=("urban-integrity-workbench-dto" "$@")
mapfile -t PKGS < <(printf '%s\n' "${PKGS_RAW[@]}" | awk '!seen[$0]++')

for pkg in "${PKGS[@]}"; do
    check_pkg "$pkg"
done
