#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# check-abbreviations.sh — fail if crates/workbench-web/src contains an
# unexpanded abbreviation in a string literal (D-04, NAV-04).
#
# The "one source, two renderers" contract this phase builds (glossary.rs's
# parser, Plan 04's `<abbr>` component, Plan 08's `/help/glossary` page) is
# only worth anything if new UI text can't silently reintroduce an
# unexplained acronym. This guard extracts every 2-to-6-letter all-caps token
# found inside a Rust string literal under crates/workbench-web/src and fails
# if a token is neither a genuine docs/abbreviations.md glossary entry nor
# listed in scripts/abbreviations-allowlist.txt.
#
# This script re-derives the same D-25 section-boundary rule
# (crates/workbench-web/src/glossary.rs's Rust parser encodes it too) in
# awk/grep, independently — two implementations of the same scope rule, not
# a shared library, per this phase's stated design.
#
# Usage:   scripts/check-abbreviations.sh          # checks the real workspace
#          scripts/check-abbreviations.sh --self-test
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

WORKBENCH_WEB_SRC="${REPO_ROOT}/crates/workbench-web/src"
GLOSSARY_MD="${REPO_ROOT}/docs/abbreviations.md"
ALLOWLIST="${REPO_ROOT}/scripts/abbreviations-allowlist.txt"

# check_tree TREE GLOSSARY_MD ALLOWLIST
#
# Fails if TREE contains a Rust string-literal token (2-6 all-caps letters) that is neither a
# genuine docs/abbreviations.md glossary entry nor listed in ALLOWLIST. A tree that does not
# exist yet is skipped with a warning, not a failure — mirrors
# check-no-analysis-literals.sh's check_tree, so this guard is safe to wire into CI immediately
# regardless of which plan in this phase has landed so far.
check_tree() {
    local tree="$1"
    local glossary_md="$2"
    local allowlist="$3"

    if [[ ! -d "$tree" ]]; then
        warn "$tree does not exist yet — nothing to check"
        return 0
    fi
    if [[ ! -f "$glossary_md" ]]; then
        die "$glossary_md not found — cannot derive the glossary token set"
    fi

    # The D-25 scope-stop: every `**TOKEN**` between "## Data sources & domain bodies" (the
    # file's first heading) and "## Requirement category codes" (excluded) — the same boundary
    # crates/workbench-web/src/glossary.rs's Rust parser encodes.
    local glossary_tokens
    glossary_tokens=$(awk '/^## Requirement category codes/{exit} /^\| \*\*/{print}' "$glossary_md" \
        | grep -oE '\*\*[A-Z0-9]{2,6}\*\*' | tr -d '*' | sort -u)

    local allowlist_tokens=""
    if [[ -f "$allowlist" ]]; then
        allowlist_tokens=$(grep -vE '^[[:space:]]*(#.*)?$' "$allowlist" | tr -d ' \t\r' | sort -u)
    fi

    # Every 2-6 letter all-caps token found inside a Rust string literal under $tree.
    #
    # Comment lines are dropped FIRST. The extraction is a bare `"[^"]*"` scan, which cannot tell a
    # string literal from a quoted phrase inside a `//` or `///` comment — so a doc comment reading
    # `the "as at" control (PIV-04)` presented `as at" control (PIV-04)` to the tokeniser and the
    # guard demanded a glossary entry for a requirement-category code that the glossary derivation
    # above **deliberately excludes** (`/^## Requirement category codes/{exit}`). D-04 is about
    # rendered UI copy; prose about the code is not copy, and a rule must not be satisfiable — or
    # breakable — by a comment describing it. The existing `SPA` allowlist entry documents itself as
    # "a Rust doc comment ... never rendered UI copy", which is this same bug paid for once already.
    #
    # Only whole lines whose first non-space characters are `//` are dropped, so a `//` inside a
    # genuine string literal is untouched. Dropping candidates can only ever make this guard
    # accept more, never reject more, so the fix cannot mask a literal it used to catch — and the
    # self-test's second half proves the drop actually happens.
    local candidate_tokens
    candidate_tokens=$(find "$tree" -name '*.rs' -type f -exec sed -e 's|^[[:space:]]*//.*$||' {} + 2>/dev/null \
        | grep -oE '"[^"]*"' \
        | grep -oE '\b[A-Z]{2,6}\b' | sort -u || true)

    local failed=0
    local token
    while IFS= read -r token; do
        [[ -z "$token" ]] && continue
        if echo "$glossary_tokens" | grep -qx "$token"; then
            continue
        fi
        if [[ -n "$allowlist_tokens" ]] && echo "$allowlist_tokens" | grep -qx "$token"; then
            continue
        fi
        local hit_file
        hit_file=$(grep -rlE "\"[^\"]*\b${token}\b[^\"]*\"" "$tree" --include='*.rs' 2>/dev/null | head -1)
        echo "  token '${token}' in ${hit_file:-$tree} is neither a docs/abbreviations.md glossary entry nor in scripts/abbreviations-allowlist.txt" >&2
        failed=1
    done <<< "$candidate_tokens"

    if [[ "$failed" -eq 1 ]]; then
        die "$tree: one or more unexpanded abbreviations found (see above) — add a docs/abbreviations.md entry or an allowlist line (D-04)"
    fi
    ok "$tree: no unexpanded abbreviation found"
}

# Hidden subcommand: lets --self-test run check_tree as a SEPARATE process (subprocess
# re-invocation of this same script) so it can assert on that process's own exit code without
# the self-test's own `set -e` treating the expected `die()` as aborting the whole self-test.
if [[ "${1:-}" == "__check_tree" ]]; then
    check_tree "$2" "$3" "$4"
    exit 0
fi

self_test() {
    info "self-test: proving check-abbreviations.sh can fail on a synthetic unexpanded abbreviation..."

    # A fixed literal here (the original 'ZZQX') can silently collide with a later, entirely
    # legitimate addition to scripts/abbreviations-allowlist.txt — abbr.rs's own defensive-
    # fallback negative test uses exactly that token (`abbr_expansion_text("ZZQX")` returns
    # `None`) and is correctly allowlisted, which quietly broke this self-test's non-vacuity
    # (found 2026-09-06, phase 43 plan 09: the self-test passed the guard's real run but failed
    # its own "did the guard actually fire" check because 'ZZQX' was, by then, an allowlisted
    # token). Generate a random 5-letter token instead and assert — loudly, before trusting the
    # result — that it collides with neither the real glossary nor the real allowlist, so this
    # assumption can never again silently rot.
    local synth_token
    # `head -c 5` closing the pipe early sends `tr` a SIGPIPE (exit 141) — with `set -o
    # pipefail` (on for this whole script) that would otherwise abort here via `set -e`. The
    # trailing `|| true` absorbs that expected, harmless pipeline status; the length check right
    # below is the real correctness guard.
    synth_token=$(LC_ALL=C tr -dc 'A-Z' </dev/urandom 2>/dev/null | head -c 5 || true)
    if [[ ${#synth_token} -ne 5 ]]; then
        die "self-test setup failed — could not generate a 5-letter synthetic token"
    fi
    if grep -qx "$synth_token" <(awk '/^## Requirement category codes/{exit} /^\| \*\*/{print}' "$GLOSSARY_MD" \
        | grep -oE '\*\*[A-Z0-9]{2,6}\*\*' | tr -d '*') \
        || { [[ -f "$ALLOWLIST" ]] && grep -qx "$synth_token" <(grep -vE '^[[:space:]]*(#.*)?$' "$ALLOWLIST" | tr -d ' \t\r'); }; then
        die "self-test setup failed — randomly generated token '${synth_token}' collided with a real glossary or allowlist entry; re-run"
    fi

    local scratch_dir
    # mktemp -d always resolves outside the repository's git working tree, never inside
    # crates/workbench-web/src — the same pattern check-no-analysis-literals.sh uses (T-43-03-02).
    scratch_dir="$(mktemp -d)"
    mkdir -p "${scratch_dir}/src"
    echo "pub const FIXTURE: &str = \"${synth_token}\";" > "${scratch_dir}/src/fixture.rs"

    local self_test_out
    self_test_out="$(mktemp)"
    if "${BASH_SOURCE[0]}" __check_tree "$scratch_dir" "$GLOSSARY_MD" "$ALLOWLIST" >"$self_test_out" 2>&1; then
        cat "$self_test_out" >&2
        rm -rf "$scratch_dir" "$self_test_out"
        die "self-test FAILED — the guard did NOT flag a synthetic '${synth_token}' abbreviation (non-vacuity broken)"
    fi
    rm -rf "$scratch_dir" "$self_test_out"

    # The other half, added 2026-09-16 with the comment-stripping fix above: a token that appears
    # ONLY inside a `//` comment must NOT fail. Without this, the fix is untested and the next
    # person to "simplify" the extractor back to a bare `grep -roE` re-introduces the false
    # positive with the guard still reporting green.
    #
    # **The token must sit INSIDE a quoted phrase in the comment**, which is the only shape the
    # bare extractor mistakes for a string literal — a first draft of this fixture put it outside
    # the quotes and the blind extractor passed it too, so restoring the bug did not redden the
    # self-test. A comment carrying a bare token was never the bug; a comment carrying a *quoted*
    # one was. Both lines below are the real 45-12 shape: a quoted phrase, then a second quoted
    # phrase holding the token.
    info "self-test: proving a quoted token inside a comment is NOT flagged..."
    scratch_dir="$(mktemp -d)"
    mkdir -p "${scratch_dir}/src"
    {
        echo "// the \"as at\" control (see \"${synth_token}-04 control\") — prose, not copy"
        echo "/// a doc comment whose \"${synth_token} phrase\" is quoted but is not copy"
        echo "pub const FIXTURE: &str = \"no abbreviation here\";"
    } > "${scratch_dir}/src/fixture.rs"

    self_test_out="$(mktemp)"
    if ! "${BASH_SOURCE[0]}" __check_tree "$scratch_dir" "$GLOSSARY_MD" "$ALLOWLIST" >"$self_test_out" 2>&1; then
        cat "$self_test_out" >&2
        rm -rf "$scratch_dir" "$self_test_out"
        die "self-test FAILED — the guard flagged '${synth_token}' where it appears only in a comment; D-04 is about rendered copy, not prose about the code"
    fi
    rm -rf "$scratch_dir" "$self_test_out"

    ok "self-test passed — the guard flags a literal and ignores a comment"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

if [[ ! -d "$WORKBENCH_WEB_SRC" ]]; then
    warn "crates/workbench-web/src does not exist yet — nothing to check"
else
    check_tree "$WORKBENCH_WEB_SRC" "$GLOSSARY_MD" "$ALLOWLIST"
fi
