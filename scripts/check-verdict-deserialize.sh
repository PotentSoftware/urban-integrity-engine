#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-verdict-deserialize.sh — fail if `Deserialize` reappears on a
# verdict-named type (WBX-04).
#
# `crates/schema/src/types/verdict.rs` deliberately never derives `Deserialize`
# on Verdict/Coverage/PopulationSource/SkipCause/Finding — a derived
# `Deserialize` impl in that module would let ANY crate construct a `Verdict`
# bypassing `classify()`'s validation (privacy in Rust is module-scoped, not
# type-scoped). That guarantee is already pinned by
# `verdict_does_not_implement_deserialize_owned` inside the crate itself; this
# script extends the same guarantee to the workbench crates, whose DTO mirror
# (`VerdictView`, see D-03) legitimately DOES derive Deserialize — but under a
# different name. A workbench crate reusing one of the bare verdict-domain
# names AND deriving Deserialize on it would silently defeat that separation.
#
# Matching is WHOLE-FILE (perl, multi-line), not line-by-line grep: a
# rustfmt-wrapped `#[derive(\n    Debug,\n    Deserialize,\n)]`, any number of
# `#[serde(...)]`/`#[cfg_attr(...)]` attribute lines between the derive and the
# `struct`/`enum`, and a hand-written `impl<'de> Deserialize<'de> for Verdict`
# are all caught — each of these evaded the original single-line `grep -A2`
# form of this guard (Phase 42 code review, WR-06), and --self-test plants one
# fixture per shape to keep it that way.
#
# Usage:   scripts/check-verdict-deserialize.sh          # checks the real workspace
#          scripts/check-verdict-deserialize.sh --self-test
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Bare verdict-domain type names — no "View" suffix. The DTO mirror
# (VerdictView, CoverageView, ...) is a strictly separate, legitimately
# Deserialize-able type; this guard only ever matches the bare domain names.
FORBIDDEN_TYPES='Verdict|Coverage|SkipCause|PopulationSource|Finding'

# derive_hits FILE TYPE_PATTERN
#
# Prints one line per `#[derive(...)]` attribute in FILE that names
# `Deserialize` AND is followed — through any run of further attributes
# (`#[serde(...)]`, `#[cfg_attr(...)]`, ...) and `//` comments — by a
# `struct`/`enum` whose name matches TYPE_PATTERN. TYPE_PATTERN='.*' means
# "any type at all" (the schema-file invariant).
#
# Anchored to `^\s*#\[derive\(` (an actual attribute at the true start of a
# line, ignoring leading whitespace) so this file's own doc comments, which
# quote "#[derive(Deserialize)]" as PROSE describing the exact danger this
# guard exists to catch, are never a false positive.
derive_hits() {
    local file="$1"
    local type_pattern="$2"
    perl -0777 -ne '
        my ($types) = @ARGV_TYPES;
        while (m{
            ^[ \t]*\#\[derive\(([^)]*)\)\]            # the derive attribute (may span lines)
            ((?:\s*(?:\#\[[^\]]*\]|//[^\n]*))*)       # any following attributes / comments
            \s*(?:pub(?:\([^)]*\))?\s+)?              # optional visibility
            (?:struct|enum)\s+([A-Za-z_][A-Za-z0-9_]*)\b
        }mgx) {
            my ($derive, $name) = ($1, $3);
            next unless $derive =~ /\bDeserialize\b/;
            next unless $name =~ /^(?:$ENV{TYPE_PATTERN})$/;
            my $line = 1 + (substr($_, 0, $-[0]) =~ tr/\n//);
            print "$ARGV:$line: $name derives Deserialize\n";
        }
    ' "$file" || true
}

# manual_impl_hits FILE TYPE_PATTERN
#
# Prints one line per hand-written `impl ... Deserialize ... for <Type>` in
# FILE whose <Type> matches TYPE_PATTERN — the most direct way to defeat the
# invariant, and one no derive-shaped check can see.
manual_impl_hits() {
    local file="$1"
    local type_pattern="$2"
    grep -nE "^[[:space:]]*impl(<[^>]*>)?[[:space:]]+(serde::)?(de::)?Deserialize(<[^>]*>)?[[:space:]]+for[[:space:]]+(${type_pattern})\b" "$file" \
        | sed "s|^|${file}:|" || true
}

# check_verdict_schema_file FILE
#
# Fails if the given file derives (or manually implements) Deserialize on ANY
# type at all — this is the crate::schema::types::verdict.rs invariant itself,
# not the workbench-side name-reuse check below.
check_verdict_schema_file() {
    local file="$1"
    if [[ ! -f "$file" ]]; then
        warn "$file does not exist — nothing to check"
        return 0
    fi
    local hit
    hit="$(TYPE_PATTERN='[A-Za-z_][A-Za-z0-9_]*' derive_hits "$file" '.*')"
    hit+="$(manual_impl_hits "$file" '[A-Za-z_][A-Za-z0-9_]*')"
    if [[ -n "$hit" ]]; then
        echo "$hit" >&2
        die "$file must never derive or implement Deserialize on any type (see this script's header)"
    fi
    ok "$file: no Deserialize derive or impl found"
}

# check_workbench_tree TREE
#
# Fails if any *.rs file under the given tree derives Deserialize on — or
# hand-implements Deserialize for — a `struct`/`enum` using one of
# FORBIDDEN_TYPES' bare names. A tree that does not exist yet is skipped with a
# warning, not a failure — the workbench crates land across several plans in
# this phase.
check_workbench_tree() {
    local tree="$1"
    if [[ ! -d "$tree" ]]; then
        warn "$tree does not exist yet — nothing to check"
        return 0
    fi
    local hit=""
    local file
    while IFS= read -r -d '' file; do
        hit+="$(TYPE_PATTERN="$FORBIDDEN_TYPES" derive_hits "$file" "$FORBIDDEN_TYPES")"
        hit+="$(manual_impl_hits "$file" "$FORBIDDEN_TYPES")"
    done < <(find "$tree" -type f -name '*.rs' -print0)
    if [[ -n "$hit" ]]; then
        echo "$hit" >&2
        die "$tree: a workbench crate defines a bare verdict-domain type name AND derives/implements Deserialize on it (see hits above)"
    fi
    ok "$tree: no type derives or implements Deserialize under a bare verdict-domain name"
}

# Hidden subcommands: let --self-test run each check as a SEPARATE process
# (subprocess re-invocation of this same script) so it can assert on that
# process's own exit code without the self-test's own `set -e` treating the
# expected `die()` as aborting the whole self-test.
if [[ "${1:-}" == "__check_workbench_tree" ]]; then
    check_workbench_tree "$2"
    exit 0
fi
if [[ "${1:-}" == "__check_verdict_schema_file" ]]; then
    check_verdict_schema_file "$2"
    exit 0
fi

# expect_tree_fails NAME FIXTURE_SOURCE
#
# Writes FIXTURE_SOURCE as the only file of a scratch tree and asserts
# check_workbench_tree FAILS on it.
expect_tree_fails() {
    local name="$1"
    local source="$2"
    local scratch_dir
    scratch_dir="$(mktemp -d)"
    mkdir -p "${scratch_dir}/src"
    printf '%s\n' "$source" > "${scratch_dir}/src/${name}.rs"
    local out
    out="$(mktemp)"
    if "${BASH_SOURCE[0]}" __check_workbench_tree "$scratch_dir" >"$out" 2>&1; then
        cat "$out" >&2
        rm -rf "$scratch_dir" "$out"
        die "self-test FAILED — the guard did NOT flag the '${name}' fixture (non-vacuity broken)"
    fi
    rm -rf "$scratch_dir" "$out"
    ok "self-test: the guard correctly flagged the '${name}' fixture"
}

# expect_tree_passes NAME FIXTURE_SOURCE
#
# The converse: a legitimate shape (the DTO mirror, prose in a doc comment)
# must NOT be flagged, or the guard would be unusable on the real tree.
expect_tree_passes() {
    local name="$1"
    local source="$2"
    local scratch_dir
    scratch_dir="$(mktemp -d)"
    mkdir -p "${scratch_dir}/src"
    printf '%s\n' "$source" > "${scratch_dir}/src/${name}.rs"
    local out
    out="$(mktemp)"
    if ! "${BASH_SOURCE[0]}" __check_workbench_tree "$scratch_dir" >"$out" 2>&1; then
        cat "$out" >&2
        rm -rf "$scratch_dir" "$out"
        die "self-test FAILED — the guard flagged the legitimate '${name}' fixture (false positive)"
    fi
    rm -rf "$scratch_dir" "$out"
    ok "self-test: the guard correctly accepted the legitimate '${name}' fixture"
}

self_test() {
    info "self-test: proving check-verdict-deserialize.sh can fail on every evasion shape..."
    # mktemp -d always resolves outside the repository's git working tree —
    # stronger than "gitignored", it is never part of the working tree at all.

    expect_tree_fails single_line_derive '
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Verdict {
    pub hit: bool,
}'

    expect_tree_fails multi_line_derive_with_attributes '
use serde::Deserialize;

#[derive(
    Debug,
    Clone,
    Deserialize,
)]
#[serde(tag = "kind")]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(PartialEq))]
pub enum Coverage {
    Full,
    Partial,
}'

    expect_tree_fails manual_impl '
use serde::{Deserialize, Deserializer};

pub struct Finding;

impl<'"'"'de> Deserialize<'"'"'de> for Finding {
    fn deserialize<D: Deserializer<'"'"'de>>(_d: D) -> Result<Self, D::Error> {
        Ok(Finding)
    }
}'

    expect_tree_fails manual_impl_fully_qualified '
pub struct SkipCause;

impl<'"'"'de> serde::Deserialize<'"'"'de> for SkipCause {
    fn deserialize<D: serde::Deserializer<'"'"'de>>(_d: D) -> Result<Self, D::Error> {
        Ok(SkipCause)
    }
}'

    expect_tree_passes dto_mirror_and_prose '
use serde::Deserialize;

/// The real danger this guard catches is a bare `#[derive(Deserialize)]` on
/// `struct Verdict` — quoted here as prose only.
#[derive(Debug, Clone, Deserialize)]
pub struct VerdictView {
    pub hit: bool,
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub hit: bool,
}'

    # The schema-file check has its own non-vacuity proof: a file with any
    # derive at all must fail it.
    local scratch_file
    scratch_file="$(mktemp --suffix=.rs)"
    printf '%s\n' '#[derive(
    Debug,
    Deserialize,
)]
pub struct Anything;' > "$scratch_file"
    local out
    out="$(mktemp)"
    if "${BASH_SOURCE[0]}" __check_verdict_schema_file "$scratch_file" >"$out" 2>&1; then
        cat "$out" >&2
        rm -f "$scratch_file" "$out"
        die "self-test FAILED — check_verdict_schema_file did NOT flag a multi-line Deserialize derive (non-vacuity broken)"
    fi
    rm -f "$scratch_file" "$out"
    ok "self-test: check_verdict_schema_file correctly flagged a multi-line Deserialize derive"

    ok "self-test passed — the guard fails on every evasion shape and accepts the legitimate ones"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

check_verdict_schema_file "${REPO_ROOT}/crates/schema/src/types/verdict.rs"

# Both workbench-facing trees per D-10's amended grep-guard scope (matches
# check-no-analysis-literals.sh's scanned trees) — either may not exist yet
# this wave; check_workbench_tree skips gracefully per-tree.
shopt -s nullglob
WORKBENCH_TREES=()
for dir in "${REPO_ROOT}"/crates/workbench*/src; do
    WORKBENCH_TREES+=("$dir")
done
shopt -u nullglob

if [[ ${#WORKBENCH_TREES[@]} -eq 0 ]]; then
    warn "no crates/workbench*/src trees exist yet — nothing to check"
else
    for tree in "${WORKBENCH_TREES[@]}"; do
        check_workbench_tree "$tree"
    done
fi
