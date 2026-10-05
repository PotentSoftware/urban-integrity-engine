#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# deploy-demo.sh — build locally, ship a FIXTURE-ONLY demo to a VPS.
#
# What this is for: giving a funder, journalist or prospective partner a URL they
# can evaluate on their own time, instead of a screen-share.
#
# What it deliberately will not do: put the live corpus on a public box.
#
#   * HMLR CCOD and OCOD are licensed to a named licensee. Serving their contents
#     to a third party through a web UI is onward disclosure — a licence breach,
#     whatever the UI looks like. See DATA-SOURCES.md.
#   * A demo screen showing a real person with a P_c score is publication to a
#     third party: a defamation event and a UK GDPR processing event, before any
#     DPIA or indemnity cover exists. The project has already recorded at least
#     one confirmed false positive against a real named individual.
#
# So: NO DATA CROSSES THE WIRE. This script ships a binary and a static frontend.
# The remote host seeds its own synthetic planted fixture from code
# (`fixture seed --planted`), which exercises every detector and carries a known
# manifest — a better demonstration than real data, because you can say "here is
# what we planted, here is what it found".
#
# Three guards enforce that, and each one aborts the deploy:
#   1. data/ is never in the rsync set (--exclude is not relied on: nothing under
#      data/ is ever added to the manifest in the first place).
#   2. The remote analysis slug must be a --fixture analysis.
#   3. Post-deploy, the remote catalog is read back and the deploy FAILS if any
#      non-fixture analysis exists on that box.
#
# Usage:
#   DEMO_HOST=root@203.0.113.10 \
#   DEMO_DOMAIN=demo.example.org \
#   DEMO_ACME_EMAIL=you@example.org \
#   scripts/deploy-demo.sh
#
#   scripts/deploy-demo.sh --add-viewer alice     # mint one viewer credential
#   scripts/deploy-demo.sh --provision            # first run: install packages
#   scripts/deploy-demo.sh --status               # what is running out there
#
# Requires locally: rsync, ssh, cargo, trunk (see scripts/build-workbench.sh).
# Requires remotely: Ubuntu 24.04 LTS, sudo, systemd. --provision does the rest.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; RESET='\033[0m'
info() { echo -e "${CYAN}[info]${RESET}  $*"; }
ok()   { echo -e "${GREEN}[ ok ]${RESET}  $*"; }
warn() { echo -e "${YELLOW}[warn]${RESET}  $*"; }
die()  { echo -e "${RED}[fail]${RESET}  $*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

REMOTE_ROOT="/opt/urban-demo"
REMOTE_ETC="/etc/urban-demo"
REMOTE_USER_ACCT="urbandemo"
SURREAL_VERSION="${SURREAL_VERSION:-3.1.0-beta.2}"
FIXTURE_LABEL="Planted fixture"

DEMO_HOST="${DEMO_HOST:-}"
DEMO_DOMAIN="${DEMO_DOMAIN:-}"
DEMO_ACME_EMAIL="${DEMO_ACME_EMAIL:-}"

MODE="deploy"
VIEWER=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --provision)   MODE="provision"; shift ;;
        --status)      MODE="status"; shift ;;
        --add-viewer)  MODE="add-viewer"; VIEWER="${2:-}"; shift 2 ;;
        --teardown)    MODE="teardown"; shift ;;
        -h|--help)     sed -n '2,45p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *)             die "unknown argument: $1 (try --help)" ;;
    esac
done

[[ -n "$DEMO_HOST" ]] || die "DEMO_HOST is not set (e.g. DEMO_HOST=root@203.0.113.10)"

rsh() { ssh -o BatchMode=yes -o ConnectTimeout=15 "$DEMO_HOST" "$@"; }

# ─────────────────────────────────────────────────────────────────────────────
# Guard 1 — refuse to run against a host that already holds a live analysis.
# Cheap, runs before anything is built, and catches the case where this script is
# pointed at a box that was once used for something else.
# ─────────────────────────────────────────────────────────────────────────────
assert_fixture_only() {
    local catalog
    catalog="$(rsh "test -x ${REMOTE_ROOT}/bin/urban-integrity-cli && cd ${REMOTE_ROOT} && ./bin/urban-integrity-cli analysis list 2>/dev/null || true")"
    [[ -z "$catalog" ]] && return 0   # nothing deployed yet — fine

    local offenders
    offenders="$(echo "$catalog" | grep -vE '^\s*$|an_planted_fixture|SLUG|LABEL|-----' || true)"
    if [[ -n "$offenders" ]]; then
        echo "$offenders" >&2
        die "remote host holds analyses that are not the planted fixture (listed above).
       This script will not deploy onto a box carrying real-corpus data.
       Inspect it, and if those analyses are genuinely disposable remove them
       by hand before re-running. See DATA-SOURCES.md for why."
    fi
    ok "remote catalog is fixture-only"
}

# ─────────────────────────────────────────────────────────────────────────────
# provision — one-time host setup
# ─────────────────────────────────────────────────────────────────────────────
do_provision() {
    [[ -n "$DEMO_DOMAIN"     ]] || die "DEMO_DOMAIN is not set (needed for TLS)"
    [[ -n "$DEMO_ACME_EMAIL" ]] || die "DEMO_ACME_EMAIL is not set (Let's Encrypt expiry notices)"

    info "checking remote OS and architecture"
    local remote_os remote_arch remote_glibc local_glibc
    remote_os="$(rsh '. /etc/os-release && echo "$ID $VERSION_ID"')"
    remote_arch="$(rsh 'uname -m')"
    [[ "$remote_arch" == "x86_64" ]] || die "remote architecture is $remote_arch; this script ships an x86_64 build"
    info "remote: $remote_os ($remote_arch)"

    # glibc forward-compatibility only. A binary built against a newer glibc than
    # the target has will fail at exec with a symbol-version error, which is an
    # obscure way to find out. Check it now instead.
    remote_glibc="$(rsh 'ldd --version | head -1 | grep -oE "[0-9]+\.[0-9]+$"')"
    local_glibc="$(ldd --version | head -1 | grep -oE '[0-9]+\.[0-9]+$')"
    if [[ "$(printf '%s\n%s\n' "$local_glibc" "$remote_glibc" | sort -V | tail -1)" != "$remote_glibc" ]]; then
        die "local glibc ($local_glibc) is newer than remote ($remote_glibc) — the binary will not exec there.
       Build in a container matching the target instead:
         docker run --rm -v \"\$PWD\":/w -w /w rust:1.96-bookworm cargo build --release -p urban-integrity-cli"
    fi
    ok "glibc: local $local_glibc <= remote $remote_glibc"

    info "installing packages, user, directories and firewall"
    rsh "sudo bash -euo pipefail -s" <<PROVISION
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl ufw debian-keyring debian-archive-keyring apt-transport-https

# Caddy from the official repo.
if ! command -v caddy >/dev/null 2>&1; then
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
        | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt \
        | tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
    apt-get update -qq
    apt-get install -y -qq caddy
fi

id -u ${REMOTE_USER_ACCT} >/dev/null 2>&1 || useradd --system --home ${REMOTE_ROOT} --shell /usr/sbin/nologin ${REMOTE_USER_ACCT}
mkdir -p ${REMOTE_ROOT}/{bin,data,dist} ${REMOTE_ETC} /var/log/caddy
chown -R ${REMOTE_USER_ACCT}:${REMOTE_USER_ACCT} ${REMOTE_ROOT}
chown -R caddy:caddy /var/log/caddy

# SurrealDB server binary. Pinned — the schema relies on beta semantics and an
# accidental upgrade here would diverge the demo from the dev machine.
if [ ! -x ${REMOTE_ROOT}/bin/surreal ]; then
    curl -fsSL https://install.surrealdb.com | sh -s -- --version ${SURREAL_VERSION} --nightly false || \
    curl -fsSL https://install.surrealdb.com | sh
    install -m 0755 "\$(command -v surreal)" ${REMOTE_ROOT}/bin/surreal
fi

# Credentials. Generated on the host, never transmitted, never root/root.
if [ ! -f ${REMOTE_ETC}/surreal.env ]; then
    PASS="\$(head -c 32 /dev/urandom | base64 | tr -d '/+=' | head -c 32)"
    printf 'SURREAL_USER=urbandemo\nSURREAL_PASS=%s\n' "\$PASS" > ${REMOTE_ETC}/surreal.env
    chmod 0640 ${REMOTE_ETC}/surreal.env
    chgrp ${REMOTE_USER_ACCT} ${REMOTE_ETC}/surreal.env
fi

ufw --force reset >/dev/null
ufw default deny incoming
ufw default allow outgoing
ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
ufw --force enable

# SSH keys only. A demo box with password auth is a demo box that gets owned.
sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config
systemctl reload ssh || systemctl reload sshd || true
PROVISION
    ok "host provisioned"
    warn "DNS: point an A record for ${DEMO_DOMAIN} at this host before the first deploy, or ACME will fail."
}

# ─────────────────────────────────────────────────────────────────────────────
# add-viewer — mint one basic-auth credential
# ─────────────────────────────────────────────────────────────────────────────
do_add_viewer() {
    [[ -n "$VIEWER" ]] || die "--add-viewer needs a name, e.g. --add-viewer alice"
    local pass hash
    pass="$(head -c 18 /dev/urandom | base64 | tr -d '/+=' | head -c 18)"
    hash="$(rsh "caddy hash-password --plaintext '$pass'")"
    rsh "sudo bash -c 'touch ${REMOTE_ETC}/viewers && grep -v \"^${VIEWER} \" ${REMOTE_ETC}/viewers > ${REMOTE_ETC}/viewers.new || true; echo \"${VIEWER} ${hash}\" >> ${REMOTE_ETC}/viewers.new; mv ${REMOTE_ETC}/viewers.new ${REMOTE_ETC}/viewers'"
    ok "viewer '${VIEWER}' added — re-run a deploy (or --provision) to render the Caddyfile"
    echo
    echo "    URL:       https://${DEMO_DOMAIN:-<domain>}/"
    echo "    Username:  ${VIEWER}"
    echo "    Password:  ${pass}"
    echo
    warn "This password is shown once and is not stored locally. Send it over a channel"
    warn "separate from the URL, and revoke it when the evaluation is over:"
    warn "  ssh ${DEMO_HOST} \"sudo sed -i '/^${VIEWER} /d' ${REMOTE_ETC}/viewers\" && scripts/deploy-demo.sh"
}

# ─────────────────────────────────────────────────────────────────────────────
# status
# ─────────────────────────────────────────────────────────────────────────────
do_status() {
    rsh "bash -s" <<'STATUS'
echo "=== services ==="
systemctl is-active urban-demo-surreal urban-demo-workbench caddy 2>/dev/null | paste -d' ' \
    <(echo -e "surreal\nworkbench\ncaddy") - || true
echo "=== memory ==="
for u in urban-demo-surreal urban-demo-workbench; do
    printf "%-26s %s\n" "$u" "$(systemctl show "$u" -p MemoryCurrent --value 2>/dev/null | numfmt --to=iec 2>/dev/null || echo n/a)"
done
echo "=== listening ==="
ss -ltnp 2>/dev/null | grep -E ':(80|443|8000|8090)\b' || echo "none"
echo "=== disk ==="
df -h /opt | tail -1
STATUS
}

# ─────────────────────────────────────────────────────────────────────────────
# teardown
# ─────────────────────────────────────────────────────────────────────────────
do_teardown() {
    warn "This stops the demo and deletes ${REMOTE_ROOT} on ${DEMO_HOST}."
    read -r -p "Type the hostname to confirm: " confirm
    [[ "$confirm" == "${DEMO_HOST#*@}" ]] || die "confirmation did not match — nothing done"
    rsh "sudo bash -c 'systemctl disable --now urban-demo-workbench urban-demo-surreal 2>/dev/null; rm -rf ${REMOTE_ROOT} ${REMOTE_ETC}; systemctl reload caddy 2>/dev/null || true'"
    ok "demo torn down"
}

# ─────────────────────────────────────────────────────────────────────────────
# deploy
# ─────────────────────────────────────────────────────────────────────────────
do_deploy() {
    [[ -n "$DEMO_DOMAIN"     ]] || die "DEMO_DOMAIN is not set"
    [[ -n "$DEMO_ACME_EMAIL" ]] || die "DEMO_ACME_EMAIL is not set"

    assert_fixture_only

    # ── build ────────────────────────────────────────────────────────────────
    # One cargo invocation at a time: rustc peaks ~3GB/process on this workspace
    # and concurrent builds have exhausted memory on the dev host before now.
    info "building the workbench frontend (Trunk)"
    scripts/build-workbench.sh

    info "building urban-integrity-cli (release)"
    cargo build --release -p urban-integrity-cli

    local bin="target/release/urban-integrity-cli"
    [[ -x "$bin" ]] || die "release binary not found at $bin"
    ok "binary: $(du -h "$bin" | cut -f1)"

    # ── render config ────────────────────────────────────────────────────────
    # Generated fresh every deploy. The operator's config.local.toml points at the
    # LIVE analysis with root/root credentials, and the committed config.toml is only
    # a fictional example; neither may ever reach this host, so the demo config is
    # built from scratch rather than copied and patched. Adapters that carry licence
    # restrictions stay off.
    local staging
    staging="$(mktemp -d)"
    trap 'rm -rf "$staging"' EXIT

    cat > "$staging/config.toml" <<'DEMOCONF'
# UrbanIntegrity demo configuration — GENERATED by scripts/deploy-demo.sh.
# Edits here are overwritten on every deploy. This host serves the synthetic
# planted fixture and nothing else.

[surreal]
ws_url = "ws://127.0.0.1:8000"
namespace = "urban_integrity"
# The analysis database is supplied per-invocation via --analysis; this default
# is a deliberately non-existent name so a command that forgets the flag fails
# loudly rather than quietly opening something real.
database = "an_unset_deliberately"
# Credentials come from URBAN__SURREAL__USERNAME / __PASSWORD (systemd env file).
username = ""
password = ""

[graph]
checkpoint_events_n = 1000
checkpoint_interval_secs = 60
checkpoint_path = "data/checkpoints"
wal_channel_capacity = 10000
wal_batch_size = 100
wal_batch_window_ms = 100
shard_count = 64
snapshot_every_n_events = 1000
snapshot_interval_secs = 30

[workbench]
bind_addr = "127.0.0.1:8090"

[ingest]
# No ingestion on the demo host. No API keys are deployed, so every network
# adapter is disabled by the empty-key path rather than by omission.
ch_api_key = ""
firecrawl_api_key = ""
cf_poll_interval_secs = 900
scrapling_url = "http://localhost"
scrapling_port = 8765
scrapling_timeout_secs = 300
planning_poll_interval_secs = 3600

[detector]
phoenix_window_days = 180
phoenix_min_value = 10000.0
# No deprivation index is shipped. Alerts carry m_env = 1.0 with a null imd_rank
# — un-weighted, and stated rather than silent.
imd_csv_path = "data/none.csv"

[enricher]
enrich_interval_secs = 86400

[enricher.ch]
api_key = ""

# Licence-restricted sources are OFF and stay off. CCOD, OCOD, ICIJ,
# OpenSanctions and OpenCorporates may not be served to third parties from this
# host under their current terms — see DATA-SOURCES.md.
[enricher.land_registry]
api_key = ""
cache_dir = "data/ccod"
ccod_refresh_interval_days = 30

[enricher.ocod]
api_key = ""
cache_dir = "data/ocod"
ocod_refresh_interval_days = 30

[enricher.london_gazette]
max_pass_secs = 1800
lookback_days = 30

[enricher.ppd]
enabled = false
years_back = 5
DEMOCONF

    # ── ship ─────────────────────────────────────────────────────────────────
    # Explicit manifest. Nothing under data/ is ever added to it — the remote host
    # generates its own corpus from code.
    info "shipping binary, frontend and config (no data)"
    rsh "rm -rf /tmp/urban-demo-stage && mkdir -p /tmp/urban-demo-stage"
    rsync -az --info=stats1 \
        "$bin" \
        "$staging/config.toml" \
        "$REPO_ROOT/deploy/demo/urban-demo-surreal.service" \
        "$REPO_ROOT/deploy/demo/urban-demo-workbench.service" \
        "$DEMO_HOST:/tmp/urban-demo-stage/"

    # ── render Caddyfile ─────────────────────────────────────────────────────
    local basicauth
    basicauth="$(rsh "sudo cat ${REMOTE_ETC}/viewers 2>/dev/null || true" | sed 's/^/\t\t/')"
    if [[ -z "$basicauth" ]]; then
        die "no viewer credentials exist yet — create one first:
       scripts/deploy-demo.sh --add-viewer <name>
       (the demo is never served without authentication)"
    fi
    python3 - "$REPO_ROOT/deploy/demo/Caddyfile.tmpl" "$staging/Caddyfile" \
             "$DEMO_DOMAIN" "$DEMO_ACME_EMAIL" "$basicauth" <<'PY'
import sys
tmpl, out, domain, email, basicauth = sys.argv[1:6]
s = open(tmpl).read()
s = s.replace('@@DOMAIN@@', domain).replace('@@ACME_EMAIL@@', email).replace('@@BASICAUTH@@', basicauth)
open(out, 'w').write(s)
PY
    rsync -az "$staging/Caddyfile" "$DEMO_HOST:/tmp/urban-demo-stage/"

    # ── install and start ────────────────────────────────────────────────────
    info "installing on the remote host"
    rsh "sudo bash -euo pipefail -s" <<INSTALL
install -m 0755 -o ${REMOTE_USER_ACCT} -g ${REMOTE_USER_ACCT} \
    /tmp/urban-demo-stage/urban-integrity-cli ${REMOTE_ROOT}/bin/urban-integrity-cli
install -m 0644 -o ${REMOTE_USER_ACCT} -g ${REMOTE_USER_ACCT} \
    /tmp/urban-demo-stage/config.toml ${REMOTE_ROOT}/config.toml
# A config.local.toml would REPLACE the generated config.toml (AppConfig::load()), so
# none may exist on the demo host.
rm -f ${REMOTE_ROOT}/config.local.toml
install -m 0644 /tmp/urban-demo-stage/urban-demo-surreal.service   /etc/systemd/system/
install -m 0644 /tmp/urban-demo-stage/urban-demo-workbench.service /etc/systemd/system/
install -m 0644 /tmp/urban-demo-stage/Caddyfile /etc/caddy/Caddyfile
rm -rf /tmp/urban-demo-stage

# Surreal credentials reach the CLI as URBAN__SURREAL__* rather than via config.toml,
# so they are never written to a file the workbench process can read as config.
. ${REMOTE_ETC}/surreal.env
printf 'URBAN__SURREAL__USERNAME=%s\nURBAN__SURREAL__PASSWORD=%s\n' "\$SURREAL_USER" "\$SURREAL_PASS" \
    > ${REMOTE_ETC}/workbench.env
chmod 0640 ${REMOTE_ETC}/workbench.env
chgrp ${REMOTE_USER_ACCT} ${REMOTE_ETC}/workbench.env

systemctl daemon-reload
systemctl enable --now urban-demo-surreal
INSTALL

    info "waiting for SurrealDB"
    rsh "for i in \$(seq 1 60); do curl -sf http://127.0.0.1:8000/health >/dev/null && exit 0; sleep 0.5; done; exit 1" \
        || die "SurrealDB did not become healthy — ssh in and check: journalctl -u urban-demo-surreal -n 50"
    ok "SurrealDB healthy"

    # ── seed the fixture, on the remote, from code ───────────────────────────
    info "seeding the planted fixture (generated remotely — no data was transmitted)"
    local slug
    slug="$(rsh "sudo -u ${REMOTE_USER_ACCT} bash -euo pipefail -s" <<SEED
set -a; . ${REMOTE_ETC}/workbench.env; set +a
cd ${REMOTE_ROOT}
CLI=./bin/urban-integrity-cli
SLUG="\$(\$CLI analysis list 2>/dev/null | awk '/${FIXTURE_LABEL}/ {print \$1; exit}' || true)"
if [ -z "\$SLUG" ]; then
    printf 'W05001492\n' > /tmp/wards.txt
    \$CLI analysis create --label '${FIXTURE_LABEL}' --wards-file /tmp/wards.txt --fixture >/dev/null
    SLUG="\$(\$CLI analysis list 2>/dev/null | awk '/${FIXTURE_LABEL}/ {print \$1; exit}')"
fi
\$CLI --analysis "\$SLUG" fixture seed --planted --reseed >/dev/null
\$CLI --analysis "\$SLUG" project >/dev/null
\$CLI --analysis "\$SLUG" probe --output data/evidence-report.md >/dev/null
echo "\$SLUG"
SEED
)"
    slug="$(echo "$slug" | tail -1 | tr -d '[:space:]')"
    [[ -n "$slug" ]] || die "fixture seeding produced no analysis slug"
    ok "fixture analysis: $slug"

    # ── guard 3 — read the catalog back ──────────────────────────────────────
    # The deploy is not considered successful until the remote box is proved to
    # hold nothing but the fixture.
    [[ "$slug" == an_planted_fixture_* ]] \
        || die "remote analysis slug '$slug' is not a planted fixture — refusing to expose it"
    assert_fixture_only

    info "starting the workbench and reloading Caddy"
    rsh "sudo bash -euo pipefail -s" <<START
grep -q '^DEMO_ANALYSIS=' ${REMOTE_ETC}/workbench.env 2>/dev/null \
    && sed -i 's/^DEMO_ANALYSIS=.*/DEMO_ANALYSIS=${slug}/' ${REMOTE_ETC}/workbench.env \
    || echo 'DEMO_ANALYSIS=${slug}' >> ${REMOTE_ETC}/workbench.env
systemctl enable --now urban-demo-workbench
systemctl restart urban-demo-workbench
caddy validate --config /etc/caddy/Caddyfile >/dev/null
systemctl reload caddy || systemctl restart caddy
START

    info "verifying the public endpoint"
    sleep 3
    local code
    code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 20 "https://${DEMO_DOMAIN}/" || echo 000)"
    case "$code" in
        401) ok "https://${DEMO_DOMAIN}/ is up and requires authentication (401 as expected)" ;;
        200) die "the endpoint answered 200 WITHOUT credentials — basic auth is not applied. Fix before sharing the URL." ;;
        000) warn "could not reach https://${DEMO_DOMAIN}/ from here — check DNS and that ACME completed: ssh ${DEMO_HOST} journalctl -u caddy -n 50" ;;
        *)   warn "unexpected status $code from https://${DEMO_DOMAIN}/ — check: ssh ${DEMO_HOST} journalctl -u caddy -n 50" ;;
    esac

    echo
    ok "demo deployed — fixture corpus only, no licensed or personal data on the host"
    echo "    URL:       https://${DEMO_DOMAIN}/"
    echo "    Analysis:  ${slug}"
    echo "    Viewers:   scripts/deploy-demo.sh --add-viewer <name>"
    echo "    Status:    scripts/deploy-demo.sh --status"
    echo "    Teardown:  scripts/deploy-demo.sh --teardown"
    echo
    warn "This box is a marketing asset, not infrastructure. Put an expiry date on it."
}

case "$MODE" in
    provision)  do_provision ;;
    add-viewer) do_add_viewer ;;
    status)     do_status ;;
    teardown)   do_teardown ;;
    deploy)     do_deploy ;;
esac
