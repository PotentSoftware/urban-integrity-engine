#!/usr/bin/env python3
"""
surreal_target.py — shared SurrealDB target resolution for the forensic scripts.

Every script in `scripts/` used to hardcode `"surreal-db": "main"`. That predates
the analysis lifecycle (v5.0): analyses now live in their own databases
(`an_<label>_<hash8>`) and every Rust CLI command selects one with a global
`--analysis` flag, bridged to `URBAN__ANALYSIS`. The Python scripts honoured
none of that, so they read and wrote a database nothing else targeted.

This module gives them the same selector, with the same precedence as the CLI:

    1. --analysis <db_name>        (explicit, wins)
    2. URBAN__ANALYSIS             (env, what the CLI bridges to)
    3. URBAN__SURREAL__DATABASE    (env, direct override)
    4. [surreal].database in config.local.toml if it exists, else config.toml
       (the same file choice as AppConfig::load(): the local file replaces the
       committed example, it is never merged with it)

**There is deliberately no default.** If none of the four resolves, this exits
with an error rather than guessing. A silent fallback is what made the old
behaviour dangerous: SurrealDB creates a database on first use, so a script
pointed at a name that does not exist writes into a fresh empty one, finds
nothing, and reports success — the same shape of silent-loss defect as the
ICIJ adapter's discarded writes and the planner-bug truncation.

For the same reason `preflight()` verifies the database actually exists before
any script does work, and names the alternatives when it does not.

Usage:

    import surreal_target

    parser = argparse.ArgumentParser()
    surreal_target.add_analysis_arg(parser)
    args = parser.parse_args()

    target = surreal_target.resolve(args)   # exits on failure
    target.preflight()                      # exits if the database is absent
    rows = target.query("SELECT * FROM entity LIMIT 1")
"""
from __future__ import annotations

import base64
import json
import os
import sys
import urllib.error
import urllib.request

_REPO_ROOT = os.path.join(os.path.dirname(__file__), "..")


def config_path() -> str:
    """config.local.toml when it exists, else config.toml — mirrors AppConfig::load()."""
    local = os.path.join(_REPO_ROOT, "config.local.toml")
    return local if os.path.isfile(local) else os.path.join(_REPO_ROOT, "config.toml")


def add_analysis_arg(parser) -> None:
    """Add the `--analysis` selector, mirroring the Rust CLI's global flag."""
    parser.add_argument(
        "--analysis",
        metavar="DB_NAME",
        help=(
            "Analysis database to target (e.g. an_example_00000000). "
            "Falls back to $URBAN__ANALYSIS, then $URBAN__SURREAL__DATABASE, "
            "then [surreal].database in config.local.toml (or config.toml)."
        ),
    )


def _read_config_toml() -> dict:
    """Minimal [surreal] reader — no tomllib dependency, matches analyst_probe.py."""
    cfg: dict = {}
    try:
        with open(config_path()) as f:
            in_surreal = False
            for line in f:
                s = line.strip()
                if s.startswith("[") and s.endswith("]"):
                    in_surreal = s == "[surreal]"
                    continue
                if in_surreal and "=" in s and not s.startswith("#"):
                    k, _, v = s.partition("=")
                    cfg[k.strip()] = v.strip().strip('"')
    except FileNotFoundError:
        pass
    return cfg


def _http_url(ws_url: str) -> str:
    u = ws_url.replace("ws://", "http://").replace("wss://", "https://")
    return u.rstrip("/") + "/sql"


def _die(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(2)


class SurrealTarget:
    """A resolved connection target: endpoint, namespace, database, credentials."""

    def __init__(self, url: str, ns: str, db: str, user: str, pw: str, source: str):
        self.url = url
        self.ns = ns
        self.db = db
        self.user = user
        self.pw = pw
        #: How `db` was chosen — printed in the banner so a run is self-documenting.
        self.source = source
        self._auth = "Basic " + base64.b64encode(f"{user}:{pw}".encode()).decode()

    # ── HTTP ────────────────────────────────────────────────────────────────

    def headers(self) -> dict:
        return {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "Authorization": self._auth,
            "surreal-ns": self.ns,
            "surreal-db": self.db,
        }

    def auth_tuple(self) -> tuple:
        """For scripts using `requests` with `auth=`."""
        return (self.user, self.pw)

    def query(self, sql: str, timeout: int = 60) -> list:
        """Run SurrealQL, return the parsed response array. Raises on transport error."""
        req = urllib.request.Request(
            self.url, data=sql.encode(), method="POST", headers=self.headers()
        )
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())

    # ── Safety ──────────────────────────────────────────────────────────────

    def preflight(self) -> None:
        """Exit unless the target database already exists.

        SurrealDB creates a database on first write, so without this a typo in
        `--analysis` silently produces an empty database and a script that
        reports finding nothing. Failing here makes a mistargeted run loud.
        """
        try:
            resp = self.query("INFO FOR NS;", timeout=30)
        except urllib.error.URLError as e:
            _die(f"SurrealDB unreachable at {self.url}: {e}\n       start it with scripts/surreal-start.sh")

        block = resp[0] if resp else {}
        if block.get("status") != "OK":
            _die(f"INFO FOR NS failed on namespace {self.ns!r}: {block.get('result')}")

        databases = (block.get("result") or {}).get("databases") or {}
        if self.db not in databases:
            available = ", ".join(sorted(databases)) or "(none)"
            _die(
                f"database {self.db!r} does not exist in namespace {self.ns!r}.\n"
                f"       resolved from: {self.source}\n"
                f"       available:     {available}\n"
                f"       Refusing to run — SurrealDB would create it empty and this\n"
                f"       script would report finding nothing."
            )

    def banner(self) -> str:
        return f"target: {self.ns}/{self.db}  ({self.source})  via {self.url}"


def resolve(args=None) -> SurrealTarget:
    """Resolve the target from --analysis / env / config.toml, or exit."""
    cfg = _read_config_toml()

    ws = os.environ.get("URBAN__SURREAL__WS_URL", cfg.get("ws_url", "ws://localhost:8000"))
    ns = os.environ.get("URBAN__SURREAL__NAMESPACE", cfg.get("namespace", "urban_integrity"))
    user = os.environ.get("URBAN__SURREAL__USERNAME", cfg.get("username", "root"))
    pw = os.environ.get("URBAN__SURREAL__PASSWORD", cfg.get("password", "root"))

    db = getattr(args, "analysis", None)
    source = "--analysis"
    if not db:
        db, source = os.environ.get("URBAN__ANALYSIS"), "$URBAN__ANALYSIS"
    if not db:
        db, source = os.environ.get("URBAN__SURREAL__DATABASE"), "$URBAN__SURREAL__DATABASE"
    if not db:
        db, source = cfg.get("database"), f"{os.path.basename(config_path())} [surreal].database"

    if not db:
        _die(
            "no analysis database selected.\n"
            "       pass --analysis <db_name>, set $URBAN__ANALYSIS, or add\n"
            "       [surreal].database to config.local.toml.\n"
            "       (there is no default — see scripts/surreal_target.py)"
        )

    return SurrealTarget(_http_url(ws), ns, db, user, pw, source)


if __name__ == "__main__":
    import argparse

    p = argparse.ArgumentParser(description="Show the resolved SurrealDB target.")
    add_analysis_arg(p)
    t = resolve(p.parse_args())
    print(t.banner())
    t.preflight()
    print("preflight: OK — database exists")
