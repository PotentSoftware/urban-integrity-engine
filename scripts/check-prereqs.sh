#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# scripts/check-prereqs.sh — report which OS prerequisites are missing, and how
# to install each one on this machine.
#
# Why: none of the prerequisites announce themselves. A missing `protoc` fails
# the build from deep inside crates/api's build script, and that one failure
# also blocks the cli, workbench and ingestor test targets. A missing SurrealDB
# is quieter still: `cargo build` and `cargo test` both succeed without it, and
# the DB-gated tests skip by returning, so the run looks green. This script
# checks everything docs/GETTING-STARTED.md step A1 lists, up front.
#
# The SurrealDB version is read from the workspace Cargo.toml pin, so the check
# cannot drift from the SDK the code is built against.
#
# Usage:
#   scripts/check-prereqs.sh               # build prerequisites + SurrealDB
#   scripts/check-prereqs.sh --build-only  # skip SurrealDB (enough for build, fmt,
#                                          # clippy and the ungated test tier)
#
# Exit status: 0 when every REQUIRED prerequisite is present, 1 otherwise.
# Optional tools (Trunk, Python, Node, a systemd user session) only ever warn.
# Nothing is installed or changed — the script only reads.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Load cargo/rustup onto PATH, as scripts/check.sh does.
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true

BUILD_ONLY=false
for arg in "$@"; do
  case "$arg" in
    --build-only) BUILD_ONLY=true ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown arg: $arg (try --build-only, --help)" >&2; exit 2 ;;
  esac
done

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
MISSING=0
ok()      { echo -e "  ${GREEN}✓${RESET} $1"; }
missing() { echo -e "  ${RED}✗${RESET} $1"; echo -e "      ${CYAN}fix:${RESET} $2"; MISSING=$((MISSING + 1)); }
warn()    { echo -e "  ${YELLOW}–${RESET} $1"; echo -e "      ${CYAN}fix:${RESET} $2"; }

have() { command -v "$1" >/dev/null 2>&1; }

# ── Which package manager are we talking to? ────────────────────────────────
# One of: apt, dnf, pacman, brew, unknown. Decides the wording of every fix line.
detect_platform() {
  if [[ "$(uname -s)" == "Darwin" ]]; then echo brew; return; fi
  local ids=""
  if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    ids="$(. /etc/os-release; echo "${ID:-} ${ID_LIKE:-}")"
  fi
  case " $ids " in
    *" debian "*|*" ubuntu "*) echo apt ;;
    *" fedora "*|*" rhel "*)   echo dnf ;;
    *" arch "*)                echo pacman ;;
    *) if have apt-get; then echo apt
       elif have dnf; then echo dnf
       elif have pacman; then echo pacman
       else echo unknown; fi ;;
  esac
}
PLATFORM="$(detect_platform)"

# pkg <apt> <dnf> <pacman> <brew> — the install command for this platform.
pkg() {
  case "$PLATFORM" in
    apt)    echo "sudo apt-get install -y $1" ;;
    dnf)    echo "sudo dnf install -y $2" ;;
    pacman) echo "sudo pacman -S --needed $3" ;;
    brew)   echo "brew install $4" ;;
    *)      echo "install '$1' (Debian name) with your package manager" ;;
  esac
}

# The SurrealDB server must match the SDK pin exactly (`surrealdb = { version = "=X" …`).
SURREAL_PIN="$(sed -n 's/^surrealdb *= *{ *version *= *"=\{0,1\}\([^"]*\)".*/\1/p' "$ROOT/Cargo.toml" | head -n 1)"

echo "UrbanIntegrity prerequisite check (package manager: ${PLATFORM})"

# ── Required to build ───────────────────────────────────────────────────────
echo -e "\n${CYAN}Build prerequisites${RESET}"

if have cc || have gcc || have clang; then ok "C compiler"
else
  if [[ "$PLATFORM" == brew ]]; then missing "C compiler" "xcode-select --install"
  else missing "C compiler" "$(pkg build-essential 'gcc gcc-c++ make' base-devel '')"; fi
fi

if have make; then ok "make"
else
  if [[ "$PLATFORM" == brew ]]; then missing "make" "xcode-select --install"
  else missing "make" "$(pkg build-essential make base-devel '')"; fi
fi

# OpenSSL headers are a Linux-only requirement: on macOS native-tls uses the
# system Security framework instead.
if [[ "$PLATFORM" != brew ]]; then
  if ! have pkg-config; then
    missing "pkg-config" "$(pkg pkg-config pkgconf-pkg-config pkgconf '')"
    missing "OpenSSL headers (cannot be checked without pkg-config)" "$(pkg libssl-dev openssl-devel openssl '')"
  else
    ok "pkg-config"
    if pkg-config --exists openssl 2>/dev/null; then ok "OpenSSL headers ($(pkg-config --modversion openssl 2>/dev/null))"
    else missing "OpenSSL headers" "$(pkg libssl-dev openssl-devel openssl '')"; fi
  fi
fi

# prost-build honours $PROTOC ahead of PATH, so the check does too.
if [[ -n "${PROTOC:-}" && -x "${PROTOC}" ]]; then ok "protoc (\$PROTOC=${PROTOC})"
elif have protoc; then ok "protoc ($(protoc --version 2>/dev/null))"
else missing "protoc — crates/api will not build, which also blocks the cli, workbench and ingestor tests" \
             "$(pkg protobuf-compiler protobuf-compiler protobuf protobuf)"; fi

for tool in curl git; do
  if have "$tool"; then ok "$tool"
  else missing "$tool" "$(pkg "$tool" "$tool" "$tool" "$tool")"; fi
done

if have cargo && have rustup; then ok "rustup + cargo (the pinned toolchain installs itself from rust-toolchain.toml)"
elif have cargo; then warn "cargo found but not rustup — rust-toolchain.toml's pinned version will not be auto-installed" \
                           "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
else missing "rustup / cargo" "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; fi

# ── SurrealDB ───────────────────────────────────────────────────────────────
if ! $BUILD_ONLY; then
  echo -e "\n${CYAN}Database${RESET}"
  SURREAL_INSTALL="curl -sSf https://install.surrealdb.com | sh -s -- --version v${SURREAL_PIN}"
  # Same lookup order the CLI uses for `analysis create --from`: SURREAL_BIN,
  # then ~/.surrealdb/surreal (what scripts/surreal-start.sh defaults to), then PATH.
  SURREAL=""
  if [[ -n "${SURREAL_BIN:-}" && -x "${SURREAL_BIN}" ]]; then SURREAL="${SURREAL_BIN}"
  elif [[ -x "$HOME/.surrealdb/surreal" ]]; then SURREAL="$HOME/.surrealdb/surreal"
  elif have surreal; then SURREAL="$(command -v surreal)"; fi

  if [[ -z "$SURREAL_PIN" ]]; then
    warn "could not read the SurrealDB pin from Cargo.toml — version not checked" \
         "check the 'surrealdb' entry under [workspace.dependencies]"
  fi
  if [[ -z "$SURREAL" ]]; then
    missing "SurrealDB server v${SURREAL_PIN} — without it the DB-gated tests skip silently and no CLI command can connect" \
            "$SURREAL_INSTALL"
  else
    SURREAL_VERSION="$("$SURREAL" version 2>/dev/null | awk 'NR==1 {print $1}')"
    if [[ -z "$SURREAL_PIN" || "$SURREAL_VERSION" == "$SURREAL_PIN" ]]; then
      ok "SurrealDB ${SURREAL_VERSION} (${SURREAL})"
      if [[ "$SURREAL" != "$HOME/.surrealdb/surreal" && -z "${SURREAL_BIN:-}" ]]; then
        warn "scripts/surreal-start.sh looks for ~/.surrealdb/surreal, not ${SURREAL}" \
             "export SURREAL_BIN=${SURREAL}   (or symlink it into ~/.surrealdb/)"
      fi
    else
      missing "SurrealDB at ${SURREAL} is ${SURREAL_VERSION:-an unreadable version}; the SDK is pinned to exactly ${SURREAL_PIN}" \
              "$SURREAL_INSTALL"
    fi
  fi
fi

# ── Optional ────────────────────────────────────────────────────────────────
echo -e "\n${CYAN}Optional${RESET}"

TRUNK_VERSION="$(trunk --version 2>/dev/null | head -n 1)"
if [[ "$TRUNK_VERSION" == "trunk 0.21.14" ]]; then ok "Trunk 0.21.14 (workbench frontend build)"
elif [[ -z "$TRUNK_VERSION" ]]; then warn "Trunk not found — only needed to build the workbench frontend" \
                                          "cargo install --locked trunk --version 0.21.14"
else warn "'trunk' on PATH reports '${TRUNK_VERSION}', expected 'trunk 0.21.14' (pg-trunk is a different tool)" \
          "cargo install --locked trunk --version 0.21.14"; fi

if have python3; then ok "python3 (Scrapling sidecar, offline scripts)"
else warn "python3 not found — only needed for the Scrapling sidecar and the offline scripts" \
          "$(pkg 'python3 python3-venv' python3 python python)"; fi

if have node && have npm; then ok "node + npm (Cosmograph shim drift guard)"
else warn "node/npm not found — only needed for scripts/check-shim-drift.sh" \
          "$(pkg 'nodejs npm' nodejs 'nodejs npm' node)"; fi

if [[ "$PLATFORM" != brew ]]; then
  if have systemd-run && systemctl --user show-environment >/dev/null 2>&1; then
    ok "systemd user session (memory-capped SurrealDB launcher)"
  else
    warn "no systemd user session — scripts/surreal-start.sh falls back to an uncapped background process" \
         "nothing to install; see docs/GETTING-STARTED.md step A4"
  fi
fi

# ── Verdict ─────────────────────────────────────────────────────────────────
echo
if [[ $MISSING -eq 0 ]]; then
  echo -e "${GREEN}All required prerequisites are present.${RESET}"
  exit 0
fi
echo -e "${RED}${MISSING} required prerequisite(s) missing${RESET} — see the fix lines above and docs/GETTING-STARTED.md step A1."
exit 1
