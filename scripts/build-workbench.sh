#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# build-workbench.sh — the ONE place Trunk runs (D-14).
#
# Trunk is a separate, explicit build step, never a build.rs side effect.
# `cargo build --workspace` and `cargo test --workspace` must keep working on
# a machine with no Trunk, no Node and no wasm target — the workbench crate
# embeds crates/workbench/dist/ via rust-embed and compiles with a stub
# "workbench assets not built" page when dist/ is absent (plan 42-05). This
# script is what actually produces that dist/ directory.
#
# Usage:   scripts/build-workbench.sh          # builds the frontend, once
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v trunk >/dev/null 2>&1; then
    die "trunk is not on PATH — install it with: cargo install --locked trunk --version 0.21.14"
fi

# A REAL, OBSERVED footgun on this exact toolchain: `pg-trunk` (a PostgreSQL extension
# manager, https://pgt.dev) also installs a binary literally named `trunk`, and
# `cargo install trunk` silently resolves to whichever `trunk` crate is already on
# crates.io under that name if a contributor typos the package. `pg-trunk`'s own
# `trunk build` subcommand means "build a PGRX/C Postgres extension" — running it
# inside crates/workbench-web would not fail loudly with a clear cause, it would
# either error confusingly (no Postgres extension manifest) or, worse, silently do
# something unrelated to the Leptos/wasm bundler this script needs. `pg-trunk`
# prints "pg-trunk X.Y.Z" for `--version`; the real (wasm-bundler) trunk prints
# "trunk X.Y.Z" with no `pg-` prefix — check for that before trusting the binary.
TRUNK_VERSION_LINE="$(trunk --version 2>&1 || true)"
if [[ "$TRUNK_VERSION_LINE" == pg-trunk* ]]; then
    die "the 'trunk' on PATH is pg-trunk (a PostgreSQL extension manager: ${TRUNK_VERSION_LINE}), not the Leptos/wasm bundler this script needs. Install the correct one with: cargo install --locked trunk --version 0.21.14 (and check PATH ordering / cargo install --list for a name collision)"
fi
if [[ "$TRUNK_VERSION_LINE" != trunk* ]]; then
    die "could not confirm 'trunk' on PATH is the Leptos/wasm bundler (got: '${TRUNK_VERSION_LINE}') — install it with: cargo install --locked trunk --version 0.21.14"
fi
ok "trunk found: ${TRUNK_VERSION_LINE}"

if ! rustup target list --installed | grep -q wasm32-unknown-unknown; then
    die "wasm32-unknown-unknown target is not installed — install it with: rustup target add wasm32-unknown-unknown"
fi
ok "wasm32-unknown-unknown target installed"

info "building crates/workbench-web with Trunk (release)..."
(cd "${REPO_ROOT}/crates/workbench-web" && trunk build --release)

ok "workbench frontend built -> ${REPO_ROOT}/crates/workbench/dist/"
