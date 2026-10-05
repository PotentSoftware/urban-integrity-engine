#!/usr/bin/env python3
"""
analyst_probe.py — Forensic corruption-evidence probe for the UrbanIntegrity graph.

Acts as a public-procurement integrity analyst.  Runs two tiers of analysis:

  Tier 1 — Graph-event probes (graph_events table)
    Motif families: Phoenix loops, address clusters, nepotism bridges.
    Plus structural/geographic signals that survive when identity is incomplete.

  Tier 2 — OSINT enrichment probes (identity_links table, Phase 16+)
    Cross-registry identity resolution: who is the applicant really?
    Confirmed links, analyst review queue, new risk node types (PSC, offshore,
    charity, property), disqualified-director edges.

The probe is adaptive: each probe emits a verdict (HIT / WEAK / DATA_GAP /
CLEAR / BLOCKED), and the orchestrator uses those verdicts to decide which
follow-up probes are worth running.

Usage:
    python3 scripts/analyst_probe.py              # probe + write evidence-report.md
    python3 scripts/analyst_probe.py --no-report  # probe only, no file written
    python3 scripts/analyst_probe.py --json out.json   # dump raw findings as JSON
                                                       # (independent of --no-report)

Connection is read from config.local.toml, else config.toml ([surreal] section) with env overrides:
    URBAN__SURREAL__WS_URL / USERNAME / PASSWORD
    URBAN__SURREAL__NAMESPACE (default urban_integrity) / DATABASE (default main)
"""

from __future__ import annotations

import json
import os
import re
import sys
import urllib.request
import urllib.error
import urllib.parse
from collections import Counter, defaultdict
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone

# ──────────────────────────────────────────────────────────────────────────
# Connection
# ──────────────────────────────────────────────────────────────────────────

def _read_config_toml() -> dict:
    """Minimal [surreal] reader — avoids a tomllib dependency on older pythons."""
    cfg: dict = {}
    # config.local.toml replaces config.toml when present — same choice as AppConfig::load().
    root = os.path.join(os.path.dirname(__file__), "..")
    path = os.path.join(root, "config.local.toml")
    if not os.path.isfile(path):
        path = os.path.join(root, "config.toml")
    try:
        with open(path) as f:
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


class SurrealClient:
    def __init__(self):
        cfg = _read_config_toml()
        ws = os.environ.get("URBAN__SURREAL__WS_URL", cfg.get("ws_url", "ws://localhost:8000"))
        self.url = _http_url(ws)
        self.user = os.environ.get("URBAN__SURREAL__USERNAME", cfg.get("username", "root"))
        self.pw = os.environ.get("URBAN__SURREAL__PASSWORD", cfg.get("password", "root"))
        self.ns = os.environ.get("URBAN__SURREAL__NAMESPACE", cfg.get("namespace", "urban_integrity"))
        # Analysis selector, matching the Rust CLI's --analysis precedence. The
        # former `"main"` default silently targeted a pre-lifecycle database that
        # nothing else reads; a probe pointed at the wrong database reports an
        # empty graph, which is indistinguishable from a real finding.
        self.db = (
            os.environ.get("URBAN__ANALYSIS")
            or os.environ.get("URBAN__SURREAL__DATABASE")
            or cfg.get("database")
        )
        if not self.db:
            print(
                "error: no analysis database selected — set $URBAN__ANALYSIS or "
                "[surreal].database in config.local.toml (or config.toml)",
                file=sys.stderr,
            )
            sys.exit(2)
        import base64
        self._auth = "Basic " + base64.b64encode(f"{self.user}:{self.pw}".encode()).decode()
        self.query_count = 0

    def query(self, sql: str) -> list:
        """Run one SurrealQL statement, return the result array of the first block."""
        self.query_count += 1
        req = urllib.request.Request(
            self.url,
            data=sql.encode(),
            method="POST",
            headers={
                "Accept": "application/json",
                "Authorization": self._auth,
                "surreal-ns": self.ns,
                "surreal-db": self.db,
                "NS": self.ns,
                "DB": self.db,
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                blocks = json.loads(resp.read().decode())
        except urllib.error.URLError as e:
            raise RuntimeError(f"SurrealDB unreachable at {self.url}: {e}")
        if not blocks or blocks[0].get("status") != "OK":
            raise RuntimeError(f"Query failed: {blocks}")
        return blocks[0].get("result", [])

    def query_bound(self, sql: str, params: dict) -> list:
        """Run one SurrealQL statement with bound parameters, return the result array
        of the first block.

        `sql` references each bound value as `$name` (matching the Rust SDK's own
        `.bind(("name", value))` convention — see land_flip.rs's T-29-04a comment).
        `params` values are passed as SurrealDB's HTTP `/sql` query-string variable
        binding (`?name=value` -> `$name`), NEVER string-interpolated into the SQL
        body — this is the mechanism that keeps untrusted values (e.g. entity_ids
        derived from scraped portal data) from ever being parsed as SurrealQL,
        regardless of what characters they contain. Verified empirically against the
        pinned SurrealDB 3.1.0-beta.2 server: a value containing `' OR 1=1; SELECT ...`
        round-trips as an inert string via `RETURN $name`, never as executed SQL.
        """
        self.query_count += 1
        qs = urllib.parse.urlencode(params)
        url = f"{self.url}?{qs}" if qs else self.url
        req = urllib.request.Request(
            url,
            data=sql.encode(),
            method="POST",
            headers={
                "Accept": "application/json",
                "Authorization": self._auth,
                "surreal-ns": self.ns,
                "surreal-db": self.db,
                "NS": self.ns,
                "DB": self.db,
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                blocks = json.loads(resp.read().decode())
        except urllib.error.URLError as e:
            raise RuntimeError(f"SurrealDB unreachable at {self.url}: {e}")
        if not blocks or blocks[0].get("status") != "OK":
            raise RuntimeError(f"Query failed: {blocks}")
        return blocks[0].get("result", [])

    def query_multi_bound(self, sql: str, params: dict) -> list:
        """Run several SurrealQL statements with bound parameters; return the RAW blocks so
        the caller can check every statement's status, not just the first. `query`/`query_bound`
        surface only block 0 — for a multi-statement write that silently hides per-statement
        rejections, which this codebase has been bitten by before."""
        self.query_count += 1
        qs = urllib.parse.urlencode(params)
        url = f"{self.url}?{qs}" if qs else self.url
        req = urllib.request.Request(
            url, data=sql.encode(), method="POST",
            headers={"Accept": "application/json", "Authorization": self._auth,
                     "surreal-ns": self.ns, "surreal-db": self.db,
                     "NS": self.ns, "DB": self.db},
        )
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                return json.loads(resp.read().decode())
        except urllib.error.URLError as e:
            raise RuntimeError(f"SurrealDB unreachable at {self.url}: {e}")

    def query_multi(self, sql: str) -> list[list]:
        """Run multiple SurrealQL statements separated by semicolons.
        Returns a list of result arrays — one per statement (skipping USE NS/DB)."""
        self.query_count += 1
        req = urllib.request.Request(
            self.url,
            data=sql.encode(),
            method="POST",
            headers={
                "Accept": "application/json",
                "Authorization": self._auth,
                "surreal-ns": self.ns,
                "surreal-db": self.db,
                "NS": self.ns,
                "DB": self.db,
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                blocks = json.loads(resp.read().decode())
        except urllib.error.URLError as e:
            raise RuntimeError(f"SurrealDB unreachable at {self.url}: {e}")
        return [b.get("result", []) for b in blocks if b.get("status") == "OK"]


# ──────────────────────────────────────────────────────────────────────────
# Findings model
# ──────────────────────────────────────────────────────────────────────────

SEV_ORDER = {"CRITICAL": 0, "HIGH": 1, "MEDIUM": 2, "LOW": 3, "INFO": 4}


@dataclass
class Finding:
    probe: str
    hypothesis: str
    verdict: str          # HIT / WEAK / DATA_GAP / CLEAR / BLOCKED
    severity: str         # CRITICAL / HIGH / MEDIUM / LOW / INFO
    summary: str
    evidence: list = field(default_factory=list)
    confidence: str = "n/a"
    tier: str = "graph"   # "graph" or "osint"


@dataclass
class Strategy:
    identity_blocked: bool = False
    osint_active: bool = False        # True if identity_links table has any rows
    confirmed_link_count: int = 0     # auto_linked + confirmed rows
    pending_review_count: int = 0
    notes: list = field(default_factory=list)

    def log(self, msg: str):
        self.notes.append(msg)


# ──────────────────────────────────────────────────────────────────────────
# Helpers — graph_events layer
# ──────────────────────────────────────────────────────────────────────────

def parse_weight(row_field: str) -> dict:
    try:
        return json.loads(row_field)
    except Exception:
        return {}


def fetch_nodes(db: SurrealClient, type_name: str, page_size: int = 20000,
                max_rows: int = 500000) -> list[dict]:
    # Paginate with ORDER BY id + LIMIT/START, advancing START by the number of rows
    # ACTUALLY returned (not by page_size). A single large LIMIT truncates the HTTP
    # response well below the requested count when weight_json is big (e.g. ~11k of ~44k
    # Contract rows), so a naive LIMIT silently samples a fraction of the data. Advancing
    # by rows-received resumes exactly where the truncated page left off.
    if not _SAFE_IDENT.match(type_name):
        raise ValueError(f"unsafe type_name: {type_name!r}")
    if not (1 <= page_size <= 100000):
        raise ValueError(f"unsafe page_size: {page_size}")
    out: list[dict] = []
    start = 0
    while start < max_rows:
        # `kind` is deliberately NOT in the WHERE clause, and is filtered in Python below.
        #
        # `kind` is indexed (idx_graph_events_kind, schema/src/db.rs). Pairing that indexed
        # equality with a CONTAINS scan on an unindexed field makes SurrealDB 3.x take the
        # index path and return a truncated, run-to-run non-deterministic row set, while
        # count() on the identical predicate stays correct. The repo pins this as a
        # regression in crates/core/tests/surreal_integration.rs ("if a future edit
        # reinstates the kind equality, this assertion catches it") — but that test guards
        # Rust, and this is Python, so it could not.
        #
        # It mattered here more than anywhere else: probe_identity_integrity computes
        # IDN-04's coverage percentage, the provenance breakdown and the self-establishing
        # D-05 regression baseline from these rows. A truncated read produces either a false
        # "coverage REGRESSED" CRITICAL on a healthy database or a falsely clean pass, and a
        # truncated FIRST run permanently enshrines a wrong baseline. fetch_edges below
        # already had the correct shape; this is now consistent with it.
        rows = db.query(
            f"SELECT id, kind, payload.entity_id AS eid, payload.weight_json AS w, timestamp "
            f"FROM graph_events "
            f"WHERE payload.weight_json CONTAINS '\"type\":\"{type_name}\"' "
            f"ORDER BY id LIMIT {page_size} START {start};"
        )
        if not rows:
            break
        for r in rows:
            if r.get("kind") != "NodeAdded":
                continue
            d = parse_weight(r.get("w", ""))
            if d:
                d["_id"] = r.get("eid")
                d["_ts"] = r.get("timestamp")
                out.append(d)
        # Advance by rows RECEIVED, pre-filter — the cursor tracks the query's row set,
        # not the subset that survived the kind filter.
        start += len(rows)
    # Dedup by entity_id (WAL is append-only; a re-ingest emits the node again).
    # Keep the latest by timestamp — matches replay's "latest wins" semantics — so
    # counts reflect the canonical graph, not raw event volume.
    return _dedup_by(out, key=lambda d: d.get("_id"), ts=lambda d: d.get("_ts"))


def _dedup_by(rows: list, key, ts=None) -> list:
    """Keep one row per key. If ts is given, keep the max-ts row; else keep first."""
    best: dict = {}
    order: list = []
    for r in rows:
        k = key(r)
        if k is None:
            continue
        if k not in best:
            best[k] = r
            order.append(k)
        elif ts is not None and (ts(r) or 0) > (ts(best[k]) or 0):
            best[k] = r
    return [best[k] for k in order]


def fetch_edges(db: SurrealClient, type_name: str, page_size: int = 20000,
                max_rows: int = 500000) -> list[dict]:
    # Same truncation hazard as fetch_nodes — large edge sets (e.g. ~40k AwardedTo)
    # overflow a single LIMIT's HTTP response. Paginate by rows-received.
    if not _SAFE_IDENT.match(type_name):
        raise ValueError(f"unsafe type_name: {type_name!r}")
    if not (1 <= page_size <= 100000):
        raise ValueError(f"unsafe page_size: {page_size}")
    out: list[dict] = []
    start = 0
    while start < max_rows:
        rows = db.query(
            f"SELECT id, payload.src_id AS src, payload.dst_id AS dst, "
            f"payload.weight_json AS w, kind, timestamp "
            f"FROM graph_events WHERE payload.weight_json CONTAINS '{type_name}' "
            f"ORDER BY id LIMIT {page_size} START {start};"
        )
        if not rows:
            break
        out.extend(
            {"src": r.get("src"), "dst": r.get("dst"), "w": r.get("w"), "ts": r.get("timestamp")}
            for r in rows if r.get("kind") == "EdgeAdded"
        )
        start += len(rows)
    # Dedup by (src,dst) — one call is a single edge type, so src+dst is the edge
    # identity; a re-ingest re-emits the same edge. Reflects the canonical graph.
    return _dedup_by(out, key=lambda e: (e.get("src"), e.get("dst")))


POSTCODE_SECTOR_RE = re.compile(r"([A-Z]{1,2}\d{1,2}[A-Z]?)\s*(\d)")
_SAFE_IDENT = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*$')


def postcode_sector(pc: str) -> str | None:
    if not pc:
        return None
    pc2 = pc.upper().strip()
    m = re.match(r"^([A-Z]{1,2}\d{1,2}[A-Z]?)\s*(\d)[A-Z]{2}$", pc2.replace(" ", ""))
    if m:
        return f"{m.group(1)} {m.group(2)}"
    return None


def _ts_to_day(ts) -> str:
    try:
        n = int(ts)
        while n > 4_102_444_800:
            n //= 1000
        return datetime.fromtimestamp(n, tz=timezone.utc).strftime("%Y-%m-%d")
    except (TypeError, ValueError):
        return str(ts)[:10]


def street_key(addr: str) -> str | None:
    if not addr:
        return None
    s = re.sub(r"^\s*\d+[A-Za-z]?\s+", "", addr).strip().lower()
    s = re.sub(r"\s+[a-z]{1,2}\d{1,2}[a-z]?\s*\d[a-z]{2}$", "", s).strip()
    return s or None


# ──────────────────────────────────────────────────────────────────────────
# Helpers — OSINT enrichment layer (identity_links table)
# ──────────────────────────────────────────────────────────────────────────

_SAFE_FILTER = re.compile(r'^[A-Za-z_][A-Za-z0-9_]*$')


def fetch_identity_links(db: SurrealClient, status_filter: str | None = None,
                          source_filter: str | None = None,
                          limit: int | None = None,
                          page_size: int = 20000,
                          max_rows: int = 500000) -> list[dict]:
    """Fetch rows from identity_links.

    status_filter: restrict to a specific status value (e.g. 'pending_review').
    source_filter: restrict to a specific adapter source (e.g. 'ch_psc', 'ch_disqualified').
    Both filters are AND-ed when both are provided.

    ``limit`` has two distinct meanings, and the difference matters because several
    callers report ``len()`` of the result as a FINDING:

      * ``None`` (default) — read every matching row, paged. Use this whenever the
        count is the answer.
      * an integer — deliberately sample the top-N by confidence in one request. Use
        this only for existence checks and fixed-size displays, never for a count.

    The default used to be a bare ``LIMIT 500`` and every caller passed its own ceiling.
    Those ceilings became samples as the data grew and were then reported as totals:
    measured 2026-08-05, `osint-psc-ownership` reported "2000 confirmed
    beneficial-ownership link(s)" against 6,666 in the table (a 70% undercount) and the
    registry probe reported "5000 identity link(s)" against 13,627. Both read as counts.
    Nobody looking at the report could tell the number was a limit.

    Paging is ORDER BY id (a stable total order — ``confidence`` has ties, so paging on
    it can skip and repeat rows) advancing by rows ACTUALLY RECEIVED, because a short
    page means the response truncated rather than the data running out. The result is
    then sorted by confidence descending, so callers that sample ``[:8]`` for evidence
    still see the strongest links. Mirrors ``fetch_nodes`` above.

    Note: the column is ``source`` (not ``adapter_name``) and ``resolved_at``
    (not ``created_at``) — matching the DDL in crates/schema/src/db.rs.
    """
    if status_filter and not _SAFE_FILTER.match(status_filter):
        raise ValueError(f"unsafe status_filter: {status_filter!r}")
    if source_filter and not _SAFE_FILTER.match(source_filter.replace("-", "_")):
        raise ValueError(f"unsafe source_filter: {source_filter!r}")
    if limit is not None and not (1 <= limit <= 100000):
        raise ValueError(f"unsafe limit: {limit}")
    if not (1 <= page_size <= 100000):
        raise ValueError(f"unsafe page_size: {page_size}")
    parts = []
    if status_filter:
        parts.append(f"status = '{status_filter}'")
    if source_filter:
        parts.append(f"source = '{source_filter}'")
    where = ("WHERE " + " AND ".join(parts)) if parts else ""
    fields = ("id, source_node_id, target_node_id, resolution_type, confidence, "
              "source, status, evidence_json, resolved_at")

    # Explicit sample: one request, top-N by confidence. Unchanged from the old shape.
    if limit is not None:
        return db.query(
            f"SELECT {fields} FROM identity_links {where} "
            f"ORDER BY confidence DESC LIMIT {limit};"
        )

    out: list[dict] = []
    start = 0
    while start < max_rows:
        rows = db.query(
            f"SELECT {fields} FROM identity_links {where} "
            f"ORDER BY id LIMIT {page_size} START {start};"
        )
        if not rows:
            break
        out.extend(rows)
        start += len(rows)
    out.sort(key=lambda r: r.get("confidence") or 0.0, reverse=True)
    return out


def fetch_enrichment_runs(db: SurrealClient) -> dict[str, dict]:
    """Fetch per-source enrichment run markers from the ``enrichment_runs`` table.

    Returns a dict keyed by the row's ``source`` (e.g. 'ch_psc') whose value is the
    full marker row. A marker's presence means the engine ran that adapter at least
    once (best-effort write from EnricherEngine, quick-260609-mhh) — even when the
    run produced zero identity_links. Its absence means the adapter never ran.

    Fully defensive and STATIC: the SELECT has no interpolation (no _SAFE_FILTER
    needed). Any error — missing table, query failure, empty result, or rows lacking
    a ``source`` key — yields ``{}`` so callers degrade to the legacy "never ran"
    behaviour rather than crashing.
    """
    try:
        rows = db.query(
            "SELECT source, last_run_at, candidates_found, auto_linked, "
            "pending_review, discarded FROM enrichment_runs;"
        )
    except Exception:
        return {}
    out: dict[str, dict] = {}
    if not rows:
        return out
    for r in rows:
        if not isinstance(r, dict):
            continue
        src = r.get("source")
        if src:
            out[src] = r
    return out


# OSINT adapter source keys whose run markers / links signal that enrichment
# executed (quick-260609-mhh — used by the node/edge coverage probes).
_OSINT_SOURCES = (
    "ch_psc", "ch_disqualified", "icij", "charity_commission",
    "land_registry_ccod", "electoral_commission", "opencorporates",
    "opensanctions", "london_gazette", "council_registers",
)

# Remediation line shared by the OSINT node/edge coverage probes: markers/links
# exist (adapters ran) yet the graph has zero OSINT nodes/edges, because the CLI
# `enrich` path drops the graph-mutation channel (`let (mutation_tx, _) = ...`).
_CLI_DROPS_MUTATION_NOTE = (
    "OSINT graph nodes/edges are materialised ONLY by the long-running `serve` "
    "enrichment path. The CLI `enrich` command drops the graph-mutation channel "
    "(`let (mutation_tx, _) = ...`), so nodes/edges produced during a CLI run are "
    "never persisted. The adapters ran and wrote identity links — run enrichment "
    "under `serve` to materialise the graph nodes/edges."
)


def osint_enrichment_ran(db: SurrealClient) -> bool:
    """True if any OSINT adapter ran (run marker present) or any identity_link exists.

    Used by the node/edge coverage probes to distinguish "adapters ran but the CLI
    dropped the graph-mutation channel" from a genuine "never ran" state. Defensive:
    any query error degrades to False (legacy "not yet run" wording).
    """
    runs = fetch_enrichment_runs(db)
    if any(src in runs for src in _OSINT_SOURCES):
        return True
    try:
        return bool(fetch_identity_links(db, limit=1))
    except Exception:
        return False


def _short_node_id(node_id: str) -> str:
    """Trim 'director:john-smith-jones-1965' to 'john-smith-jones-1965' for display."""
    if node_id and ":" in node_id:
        return node_id.split(":", 1)[1]
    return node_id or "(unknown)"


def _confidence_bar(c: float, width: int = 10) -> str:
    filled = int(round(c * width))
    return "█" * filled + "░" * (width - filled)


def _parse_evidence(ev_json: str) -> dict:
    try:
        return json.loads(ev_json) if ev_json else {}
    except Exception:
        return {}


# ──────────────────────────────────────────────────────────────────────────
# Tier-1 probes — graph_events
# ──────────────────────────────────────────────────────────────────────────

def probe_precomputed_alerts(db: SurrealClient, st: Strategy) -> Finding:
    rows = db.query("SELECT count() AS n FROM graph_events WHERE kind='MotifDetected' GROUP ALL;")
    n = rows[0]["n"] if rows else 0
    if n > 0:
        # Sample the top alert types for richer evidence
        type_rows = db.query(
            "SELECT payload.motif_type AS t, count() AS n "
            "FROM graph_events WHERE kind='MotifDetected' "
            "GROUP BY payload.motif_type ORDER BY n DESC LIMIT 8;"
        )
        ev = [f"{r.get('n', 0):>4}  {r.get('t', 'unknown')}" for r in type_rows] if type_rows else []
        return Finding(
            "precomputed-alerts",
            "The detector engine has already persisted MotifDetected alerts.",
            "HIT", "HIGH",
            f"{n} pre-computed motif alert(s) — review via `urban-integrity alerts list`.",
            evidence=ev,
            confidence="high",
        )
    st.log("No MotifDetected events: all findings below are derived by direct forensic query.")
    return Finding(
        "precomputed-alerts",
        "The detector engine has already persisted MotifDetected alerts.",
        "CLEAR", "INFO",
        "Zero MotifDetected events. Forensic analysis proceeds from raw nodes/edges.",
        confidence="high",
    )


# D-05: how many percentage points a re-measurement may drop below the stored
# baseline before the probe treats it as a real regression rather than noise.
# Mirrors AUTO_LINK_THRESHOLD's role in crates/enricher/src/confidence.rs as a
# named, documented tolerance rather than a magic number.
REGRESSION_TOLERANCE_PTS = 1.0

# The four buckets probe_identity_integrity always reports, in display order.
# "not_yet_measured" is distinct from "absent_at_source": it means 24-05's
# backfill (or 24-02-era ingest) has not yet written an applicant_provenance
# value at all, not that the portal was confirmed to have no applicant.
_PROVENANCE_BUCKETS = ("published", "absent_at_source", "fetch_failed", "not_yet_measured")


def _catalog_query(client: SurrealClient, sql: str) -> list:
    """Issue one query against the `_catalog` database using `client`'s connection
    credentials (url/auth/namespace), with the `surreal-db`/`DB` headers hardcoded
    to the literal `_catalog` — NOT `client.db` (the current analysis database).

    Deliberately does not instantiate a second SurrealClient: that would re-read
    config.toml/env and could silently pick a different DB than the one `client`
    is already connected to.
    """
    req = urllib.request.Request(
        client.url,
        data=sql.encode(),
        method="POST",
        headers={
            "Accept": "application/json",
            "Authorization": client._auth,
            "surreal-ns": client.ns,
            "surreal-db": "_catalog",
            "NS": client.ns,
            "DB": "_catalog",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            blocks = json.loads(resp.read().decode())
    except urllib.error.URLError as e:
        raise RuntimeError(f"_catalog query unreachable at {client.url}: {e}")
    if not blocks or blocks[0].get("status") != "OK":
        raise RuntimeError(f"_catalog query failed: {blocks}")
    return blocks[0].get("result", [])


def read_coverage_baseline(client: SurrealClient) -> dict | None:
    """Read the D-05 regression baseline for `client.db` from `_catalog.analyses`.

    Degrade-safe: any query error, missing row, or malformed JSON returns None —
    matches `osint_enrichment_ran`'s "any query error degrades to a safe default"
    convention. `client.db` is validated against `_SAFE_IDENT` before being
    embedded in the SQL string even though it originates from local config/env,
    not portal-derived input (defense in depth, same precedent as `type_name` in
    `fetch_nodes`).
    """
    if not _SAFE_IDENT.match(client.db):
        raise ValueError(f"unsafe db_name: {client.db!r}")
    try:
        rows = _catalog_query(
            client,
            f"SELECT coverage_baseline_json FROM analyses WHERE db_name = '{client.db}' LIMIT 1;",
        )
    except Exception:
        return None
    if not rows:
        return None
    raw = rows[0].get("coverage_baseline_json")
    if not raw:
        return None
    try:
        return json.loads(raw)
    except Exception:
        return None


def write_coverage_baseline(client: SurrealClient, db_name: str, payload: dict) -> None:
    """Self-establish/update the D-05 regression baseline for `db_name`.

    This is the Python-side writer so the probe itself can self-establish the
    first baseline; `crates/cli/src/registry.rs::set_coverage_baseline` is the
    canonical writer for CLI-driven writes. `db_name` is validated against
    `_SAFE_IDENT` before being embedded in the SQL string — same accepted
    exception as `read_coverage_baseline` and `type_name` in `fetch_nodes`.
    """
    if not _SAFE_IDENT.match(db_name):
        raise ValueError(f"unsafe db_name: {db_name!r}")
    raw = json.dumps(payload).replace("\\", "\\\\").replace("'", "\\'")
    _catalog_query(
        client,
        f"UPDATE type::record('analyses', '{db_name}') SET coverage_baseline_json = '{raw}';",
    )


def probe_identity_integrity(db: SurrealClient, st: Strategy) -> Finding:
    contracts = [c for c in fetch_nodes(db, "Contract")
                 if (c.get("_id") or "").startswith("planning:")]
    total = len(contracts)
    if total == 0:
        st.identity_blocked = True
        return Finding(
            "identity-integrity",
            "Applicants/awardees are identifiable so identity motifs can run.",
            "DATA_GAP", "HIGH",
            "No planning Contract nodes present — identity-based detection cannot run.",
            confidence="high",
        )
    provenance_counts = Counter(
        c.get("applicant_provenance") or "not_yet_measured" for c in contracts
    )
    pct_published = 100.0 * provenance_counts.get("published", 0) / total
    breakdown_ev = [f"{provenance_counts.get(b, 0):>6}  {b}" for b in _PROVENANCE_BUCKETS]
    breakdown_text = ", ".join(f"{b}={provenance_counts.get(b, 0)}" for b in _PROVENANCE_BUCKETS)

    baseline = read_coverage_baseline(db)
    if baseline is not None and pct_published < baseline["pct_published"] - REGRESSION_TOLERANCE_PTS:
        st.identity_blocked = True
        return Finding(
            "identity-integrity",
            "Applicants/awardees are identifiable so identity motifs can run.",
            "DATA_GAP", "CRITICAL",
            f"Applicant-provenance coverage REGRESSED: {pct_published:.1f}% published now vs "
            f"{baseline['pct_published']:.1f}% at baseline (recorded "
            f"{baseline.get('recorded_at', 'unknown')}). Breakdown of {total} planning "
            f"Contract(s): {breakdown_text}.",
            evidence=breakdown_ev,
            confidence="high",
        )

    # Ratchet up on improvement (or self-establish a first baseline); never
    # silently ratchet down once a regression check against a real baseline
    # has already passed above.
    if baseline is None or pct_published > baseline["pct_published"]:
        write_coverage_baseline(
            db, db.db,
            {
                "pct_published": pct_published,
                "total": total,
                "recorded_at": datetime.now(timezone.utc).isoformat(),
                "breakdown": dict(provenance_counts),
            },
        )

    return Finding(
        "identity-integrity",
        "Applicants/awardees are identifiable so identity motifs can run.",
        "CLEAR", "INFO",
        f"Applicant-provenance coverage: {pct_published:.1f}% published of {total} planning "
        f"Contract(s): {breakdown_text}.",
        evidence=breakdown_ev,
        confidence="high",
    )


# Every term each motif detector requires, keyed by the term's short id. A motif can only
# fire when ALL of its terms are non-zero; reporting only the missing *node types* hid the
# fact that Phoenix is short two terms, not one (Officer nodes AND dissolved counterparties).
#
# `probe` is either:
#   ("derived", "<key>")            — computed in `_count_motif_term`, for terms whose
#                                     truth needs endpoint identity, not a string match
#   (event_kind, [clause, ...])     — clauses ANDed onto a `count() GROUP ALL`
#
# Terms are matched on what the detector actually traverses, not on a convenient string.
# `Directs` edges are the cautionary case: `director:{slug}` ids belong to **Entity** nodes
# minted for planning applicants (`map_planning_item`), not to `Officer` nodes, and the
# council-registers adapter emits councillor→company `Directs` too. Counting either as
# Phoenix's officer hop reports the term satisfied while Phoenix still cannot traverse it.
#
# Source of truth for each term is cited so a reader can check the claim against the
# detector rather than trusting this table.
_TERMINAL_STATUS_CLAUSE = (
    # EntityStatus is `#[serde(tag = "type", content = "content")]`, so a terminal company
    # serialises "status":{"type":"Dissolved","content":"..."}. Active carries no content
    # and is the serde default — which is exactly why a company whose real status was
    # never fetched is indistinguishable from a genuinely live one.
    "(payload.weight_json CONTAINS '\"type\":\"Dissolved\"' "
    "OR payload.weight_json CONTAINS '\"type\":\"InLiquidation\"')"
)

_MOTIF_TERMS: dict = {
    "Phoenix loop": {
        "source": "crates/detector/src/motif/phoenix.rs:57-131",
        "terms": [
            ("officer-nodes", "Officer nodes",
             ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"Officer\"'"]),
             "the seed's officer hop"),
            ("officer-directs-edges", "Directs edges touching an Officer node",
             ("derived", "officer-directs"),
             "connects officers to their companies"),
            ("terminal-status", "Dissolved/InLiquidation entities",
             ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"Entity\"'",
                            _TERMINAL_STATUS_CLAUSE]),
             "the failed counterparty the loop points at"),
            ("awardedto-edges", "AwardedTo edges",
             ("EdgeAdded", ["payload.weight_json CONTAINS '\"type\":\"AwardedTo\"'"]),
             "supplies the award date anchoring the 180-day window"),
        ],
    },
    "Nepotism bridge": {
        "source": "crates/detector/src/motif/nepotism.rs:29-60",
        "terms": [
            ("political-actor-nodes", "PoliticalActor nodes",
             ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"PoliticalActor\"'"]),
             "the BFS start point"),
            ("officer-nodes", "Officer nodes",
             ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"Officer\"'"]),
             "the BFS target"),
            ("political-actor-edges", "Edges incident to a PoliticalActor",
             # Councillors are keyed `political_actor:{slug}` (council_registers) and
             # party recipients `PA:{slug}` (electoral_commission). A PoliticalActor with
             # no incident edge has nowhere to BFS to. Prefix-anchored, not CONTAINS, so
             # an unrelated id that happens to embed the marker cannot satisfy the term.
             ("EdgeAdded", ["(string::starts_with(payload.src_id, 'political_actor:') "
                            "OR string::starts_with(payload.dst_id, 'political_actor:') "
                            "OR string::starts_with(payload.src_id, 'PA:') "
                            "OR string::starts_with(payload.dst_id, 'PA:'))"]),
             "the only edges the BFS can leave the start point on"),
        ],
    },
    "Address cluster": {
        "source": "crates/detector/src/motif/cluster.rs:107-190",
        "terms": [
            ("entity-nodes", "Entity nodes",
             ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"Entity\"'"]),
             "the co-located bidders"),
            ("location-nodes", "Location nodes",
             ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"Location\"'"]),
             "the shared address"),
            ("locatedat-edges", "LocatedAt edges",
             ("EdgeAdded", ["payload.weight_json CONTAINS '\"type\":\"LocatedAt\"'"]),
             "binds bidders to addresses"),
        ],
    },
}


def _count_motif_term(db: SurrealClient, probe: tuple) -> int:
    """Count graph_events rows matching one motif term.

    `count() ... GROUP ALL` is safe under the SurrealDB 3.x planner bug (counts stay
    correct where row-returning selects truncate) — see the note on `fetch_nodes`.
    """
    kind, spec = probe

    if kind == "derived":
        if spec == "officer-directs":
            # Resolve real Officer node ids, then count Directs edges incident to one.
            # No Officers means no such edge can exist, so skip the edge scan entirely —
            # which is also the current state, making this term free until Phase 26 lands.
            officers = {n["_id"] for n in fetch_nodes(db, "Officer") if n.get("_id")}
            if not officers:
                return 0
            return sum(
                1 for e in fetch_edges(db, "Directs")
                if e.get("src") in officers or e.get("dst") in officers
            )
        raise ValueError(f"unknown derived term: {spec!r}")

    where = " AND ".join([f"kind='{kind}'"] + list(spec))
    rows = db.query(f"SELECT count() AS n FROM graph_events WHERE {where} GROUP ALL;")
    return rows[0]["n"] if rows else 0


def _dissolution_date_spread(db: SurrealClient, sample_limit: int = 20000) -> tuple:
    """Distinct dissolution dates over a bounded sample → (distinct_count, sampled).

    A term can be non-zero and still useless. Phoenix windows on
    `|award_date - dissolved_dt| <= 180 days` (`phoenix.rs:119-127`), so the dissolution
    date is load-bearing, not decorative. The company-profile stream reports *that* a
    company's status changed, not when it ceased, and the mapper stamps the observation
    time — so a stream-only corpus has every dissolution dated identically and the window
    is anchored at ingest day rather than at cessation.

    `kind` is filtered in Python, not in the WHERE clause — see `fetch_nodes`.
    """
    rows = db.query(
        "SELECT id, kind, payload.weight_json AS w FROM graph_events "
        "WHERE payload.weight_json CONTAINS '\"type\":\"Dissolved\"' "
        f"ORDER BY id LIMIT {sample_limit};"
    )
    dates = set()
    sampled = 0
    for r in rows:
        if r.get("kind") != "NodeAdded":
            continue
        m = re.search(r'"type":"Dissolved","content":"([^"]{10})', r.get("w") or "")
        if m:
            dates.add(m.group(1))
            sampled += 1
    return len(dates), sampled


def probe_detector_readiness(db: SurrealClient, st: Strategy) -> Finding:
    osint_types = ("Psc", "Charity", "Property", "OffshoreEntity")

    # Count each distinct term once, then fan the result out to every motif using it —
    # Officer nodes are a term of both Phoenix and nepotism.
    term_counts: dict = {}
    term_labels: dict = {}
    for spec in _MOTIF_TERMS.values():
        for key, label, probe, _role in spec["terms"]:
            if key not in term_counts:
                term_counts[key] = _count_motif_term(db, probe)
                term_labels[key] = label

    osint_counts = {
        t: _count_motif_term(
            db, ("NodeAdded", [f"payload.weight_json CONTAINS '\"type\":\"{t}\"'"])
        )
        for t in osint_types
    }
    osint_present = [t for t, n in osint_counts.items() if n > 0]

    # A term can be satisfied on count and still unusable. Track those separately: they do
    # not make a motif "blocked" (the data is there), but reporting them green would repeat
    # the mistake this probe was rewritten to fix — describing a detector as ready when it
    # cannot produce a correct result.
    degraded: dict = {}
    if term_counts.get("terminal-status", 0) > 0:
        distinct_dates, sampled = _dissolution_date_spread(db)
        if distinct_dates == 1 and sampled > 1:
            degraded["terminal-status"] = (
                f"all {sampled} sampled dissolutions share one date — a stream-arrival "
                "stamp, not date_of_cessation, so Phoenix's 180-day window is anchored at "
                "ingest day"
            )

    # Evidence: every term grouped under the motif that needs it, so a blocked motif's
    # full shortfall is visible at a glance rather than inferred from a node-type list.
    ev: list = []
    blocked: list = []
    for motif, spec in _MOTIF_TERMS.items():
        missing = [key for key, _l, _p, _r in spec["terms"] if term_counts[key] == 0]
        state = f"BLOCKED — {len(missing)} of {len(spec['terms'])} terms missing" if missing \
            else "all terms present"
        ev.append(f"{motif} ({spec['source']}) — {state}")
        for key, label, _probe, role in spec["terms"]:
            if term_counts[key] == 0:
                mark = "MISSING"
            elif key in degraded:
                mark = "DEGRADED"
            else:
                mark = "ok"
            ev.append(f"  {term_counts[key]:>8}  {label:<38} {mark:<8} {role}")
            if key in degraded:
                ev.append(f"{'':>10}  {'':<38} {'':<8} ^ {degraded[key]}")
        if missing:
            blocked.append(
                f"{motif} (missing: {', '.join(term_labels[k] for k in missing)})"
            )
    ev.append("OSINT node types")
    for t in osint_types:
        ev.append(f"  {osint_counts[t]:>8}  {t}")

    if blocked:
        all_missing = sorted({
            term_labels[key]
            for spec in _MOTIF_TERMS.values()
            for key, _l, _p, _r in spec["terms"]
            if term_counts[key] == 0
        })
        summary = (
            f"{len(blocked)} of {len(_MOTIF_TERMS)} motif detectors cannot fire. "
            f"Missing terms: {', '.join(all_missing)}. "
            f"Blocked: {'; '.join(blocked)}. "
            "A motif needs every one of its terms — supplying one missing term does not "
            "unblock a motif that is short two."
        )
        if degraded:
            summary += (
                " Degraded terms (present but not usable): "
                + "; ".join(f"{term_labels[k]} — {why}" for k, why in degraded.items())
                + "."
            )
        if osint_present:
            summary += f" OSINT types present: {', '.join(osint_present)}."
        return Finding(
            "detector-readiness",
            "Every term of every motif detector is present, so all three can fire.",
            "DATA_GAP", "HIGH", summary, evidence=ev, confidence="high",
        )

    summary = "All motif terms present — all three detectors can fire."
    if degraded:
        summary += (
            " Degraded terms (present but not usable): "
            + "; ".join(f"{term_labels[k]} — {why}" for k, why in degraded.items())
            + "."
        )
    if osint_present:
        summary += f" OSINT enrichment types active: {', '.join(osint_present)}."
    else:
        summary += (" OSINT node types (PSC, Charity, Property, OffshoreEntity) absent — "
                    "Phase 17+ not yet run.")
    return Finding(
        "detector-readiness",
        "Every term of every motif detector is present, so all three can fire.",
        "CLEAR", "INFO", summary, evidence=ev, confidence="high",
    )


def probe_shared_exact_address(db: SurrealClient, st: Strategy) -> Finding:
    locs = {n["_id"]: n for n in fetch_nodes(db, "Location")}
    located = fetch_edges(db, "LocatedAt")
    # post-D-04: a planning Contract's LocatedAt edge is sourced from the Contract node, not
    # the applicant Entity, so LocatedAt.src can be a contract id rather than a director/entity
    # id. Resolve it back to the applicant via AwardedTo (src: entity (applicant), dst:
    # contract) before grouping, so this probe keeps counting distinct APPLICANTS rather than
    # silently counting distinct APPLICATIONS under an unchanged label.
    awarded = fetch_edges(db, "AwardedTo")
    contract_to_applicant = {e["dst"]: e["src"] for e in awarded if e.get("src") and e.get("dst")}
    by_loc: dict = defaultdict(set)
    for e in located:
        if e["dst"] and e["src"]:
            applicant = contract_to_applicant.get(e["src"], e["src"])
            by_loc[e["dst"]].add(applicant)
    shared = {loc: apps for loc, apps in by_loc.items() if len(apps) >= 2}
    if not shared and st.identity_blocked:
        return Finding(
            "address-cluster-exact",
            "Multiple distinct applicants share one exact registered address (shell-ring signal).",
            "BLOCKED", "INFO",
            "Cannot evaluate: applicant identity is broken, so distinct-applicants-per-address "
            "is unmeasurable. Pivoting to geographic clustering.",
            confidence="low",
        )
    if shared:
        ev = []
        for loc, apps in sorted(shared.items(), key=lambda kv: -len(kv[1]))[:10]:
            name = locs.get(loc, {}).get("name", loc)
            ev.append(f"{len(apps):>3} applicants @ {name}")
        return Finding(
            "address-cluster-exact",
            "Multiple distinct applicants share one exact registered address (shell-ring signal).",
            "HIT", "HIGH",
            f"{len(shared)} address(es) shared by 2+ distinct applicants — possible shell-company ring.",
            evidence=ev, confidence="medium",
        )
    return Finding(
        "address-cluster-exact",
        "Multiple distinct applicants share one exact registered address (shell-ring signal).",
        "CLEAR", "INFO", "No exact address shared by multiple distinct applicants.",
        confidence="medium",
    )


def probe_geographic_concentration(db: SurrealClient, st: Strategy) -> Finding:
    locs = fetch_nodes(db, "Location")
    sectors: Counter = Counter()
    streets: Counter = Counter()
    for n in locs:
        sec = postcode_sector(n.get("postcode") or "")
        if sec:
            sectors[sec] += 1
        sk = street_key(n.get("address_line_1") or n.get("name") or "")
        if sk:
            streets[sk] += 1
    repeat_streets = [(k, v) for k, v in streets.most_common() if v >= 2]
    top_sectors = sectors.most_common(8)
    ev = []
    if top_sectors:
        ev.append("Top postcode sectors by application count:")
        ev += [f"  {v:>3}  {k}" for k, v in top_sectors]
    if repeat_streets:
        ev.append("Streets with ≥2 applications:")
        ev += [f"  {v:>3}  {k}" for k, v in repeat_streets[:10]]
    if repeat_streets:
        return Finding(
            "geographic-concentration",
            "Development activity clusters by street/postcode sector (coordinated activity signal).",
            "WEAK", "MEDIUM",
            f"{len(repeat_streets)} street(s) carry multiple applications, "
            f"{len(top_sectors)} postcode sector(s) show concentration. Without applicant "
            "identity these cannot be tied to a single beneficiary.",
            evidence=ev, confidence="low",
        )
    return Finding(
        "geographic-concentration",
        "Development activity clusters by street/postcode sector (coordinated activity signal).",
        "CLEAR", "INFO", "No notable street-level repetition; geographically dispersed.",
        evidence=ev, confidence="medium",
    )


def probe_ward_concentration(db: SurrealClient, st: Strategy) -> Finding:
    # Ward attribution lives on the Ward→Contract "Represents" edge (src = ward:{code}).
    # We aggregate server-side rather than pulling Contract nodes client-side: a Contract's
    # weight_json is large, so `fetch_nodes` truncates the HTTP response well below its LIMIT
    # (~11k of ~44k contracts observed), silently sampling ~25% of the data. GROUP BY on the
    # edge counts every application exactly once, with no truncation.
    # Count DISTINCT (ward, contract) edges — an append-only re-ingest re-emits the
    # same Represents edge, so a plain count() over-reports. Inner GROUP BY dedups the
    # edge; outer counts distinct contracts per ward.
    # CR-02: the inner SELECT returns rows, so it must not pair the indexed `kind`
    # equality with the CONTAINS scan (see fetch_nodes). `kind` is selected and filtered
    # in the outer HAVING-equivalent below rather than in the WHERE clause.
    rows = db.query(
        "SELECT ward, count() AS n FROM "
        "(SELECT payload.src_id AS ward, payload.dst_id AS contract, kind FROM graph_events "
        " WHERE payload.weight_json CONTAINS 'Represents' "
        " GROUP BY ward, contract, kind) "
        "WHERE kind = 'EdgeAdded' "
        "GROUP BY ward ORDER BY n DESC;"
    )
    wards: Counter = Counter()
    for r in rows:
        w = r.get("ward")
        if not w:
            continue
        code = w[len("ward:"):] if w.startswith("ward:") else w
        wards[code] += int(r.get("n") or 0)
    total_attributed = sum(wards.values())

    # Contracts carrying no ward attribution (e.g. Contracts Finder awards without a ward edge).
    # DISTINCT contract entity_ids — raw NodeAdded events over-count after re-ingests.
    crows = db.query(
        "SELECT count() AS n FROM "
        "(SELECT payload.entity_id FROM graph_events WHERE kind='NodeAdded' "
        " AND payload.weight_json CONTAINS '\"type\":\"Contract\"' GROUP BY payload.entity_id) "
        "GROUP ALL;"
    )
    total_contracts = crows[0]["n"] if crows else 0
    no_ward = max(0, total_contracts - total_attributed)

    ev = [f"  {v:>4}  {k}" for k, v in wards.most_common(10)]
    if no_ward:
        ev.append(f"  {no_ward:>4}  (no ward attribution)")
    detail = f"{len(wards)} ward(s) across {total_attributed} ward-attributed applications"
    if total_contracts:
        detail += f" of {total_contracts} contracts total."
    else:
        detail += "."
    if no_ward:
        detail += f" {no_ward} lack ward attribution — limits IMD weighting."
    return Finding(
        "ward-concentration",
        "Applications concentrate in particular wards (cross with IMD deprivation for P_c scoring).",
        "WEAK" if wards else "DATA_GAP", "LOW",
        detail,
        evidence=ev, confidence="low" if wards else "high",
    )


def probe_temporal_burst(db: SurrealClient, st: Strategy) -> Finding:
    contracts = fetch_nodes(db, "Contract")
    days: Counter = Counter()
    for c in contracts:
        ts = c.get("_ts")
        if not ts:
            continue
        days[_ts_to_day(ts)] += 1
    if not days:
        return Finding(
            "temporal-burst",
            "Applications arrive in abnormal same-day bursts (batch submission signal).",
            "DATA_GAP", "INFO",
            "No usable timestamps on contract nodes — temporal analysis not possible.",
            confidence="high",
        )
    top = days.most_common(5)
    ev = [f"  {v:>3} applications  {k}" for k, v in top]
    return Finding(
        "temporal-burst",
        "Applications arrive in abnormal same-day bursts (batch submission signal).",
        "WEAK" if top and top[0][1] >= 10 else "CLEAR", "LOW",
        f"Busiest ingestion day: {top[0][1]} applications. "
        "Caveat: graph-ingestion timestamps, not portal submission dates.",
        evidence=ev, confidence="low",
    )


# ──────────────────────────────────────────────────────────────────────────
# Tier-2 probes — OSINT enrichment layer
# ──────────────────────────────────────────────────────────────────────────

def probe_identity_link_registry(db: SurrealClient, st: Strategy) -> Finding:
    """OSINT layer health check: how many identity resolutions exist and at what tier?"""
    all_links = fetch_identity_links(db)  # count is the finding — read all
    total = len(all_links)

    if total == 0:
        runs = fetch_enrichment_runs(db)
        if runs:
            # Adapters ran but produced zero links — registry coverage gap, not "never ran".
            ran = ", ".join(sorted(runs)) or "(none)"
            return Finding(
                "osint-registry",
                "The identity_links table holds resolved cross-registry entity matches.",
                "CLEAR", "INFO",
                "identity_links table is empty, but enrichment HAS run: run markers exist "
                f"for {ran}. The adapter(s) executed and found zero cross-registry overlap "
                "with the current graph — a data-coverage gap, not a missing run. "
                "Re-running enrich will not create links until the registries intersect "
                "entities already in the graph.",
                confidence="high",
                tier="osint",
            )
        return Finding(
            "osint-registry",
            "The identity_links table holds resolved cross-registry entity matches.",
            "DATA_GAP", "MEDIUM",
            "identity_links table is empty. OSINT enrichment has not run yet — "
            "execute `urban-integrity enrich --source all` after wiring Phase 17+ adapters "
            "(CH PSC, Disqualified Directors, Land Registry, Charity Commission).",
            confidence="high",
            tier="osint",
        )

    status_counts: Counter = Counter(r.get("status", "unknown") for r in all_links)
    adapter_counts: Counter = Counter(r.get("source", "unknown") for r in all_links)
    confirmed = status_counts.get("confirmed", 0) + status_counts.get("auto_linked", 0)
    pending = status_counts.get("pending_review", 0)
    candidate = status_counts.get("candidate", 0)
    rejected = status_counts.get("rejected", 0)

    st.osint_active = True
    st.confirmed_link_count = confirmed
    st.pending_review_count = pending

    confidences = [r.get("confidence", 0) for r in all_links if r.get("confidence") is not None]
    avg_conf = sum(confidences) / len(confidences) if confidences else 0

    ev = [
        f"Status distribution:",
        f"  auto_linked / confirmed : {confirmed:>5}  (≥0.85 confidence — actionable intelligence)",
        f"  pending_review          : {pending:>5}  (0.70–0.84 confidence — needs analyst sign-off)",
        f"  candidate               : {candidate:>5}  (<0.70 confidence — low-priority)",
        f"  rejected                : {rejected:>5}",
        f"",
        f"Average confidence (all links): {avg_conf:.3f}",
        f"",
        f"Top adapters:",
    ]
    for adapter, n in adapter_counts.most_common(5):
        ev.append(f"  {n:>5}  {adapter}")

    verdict = "HIT" if confirmed > 0 else ("WEAK" if pending > 0 else "CLEAR")
    severity = "HIGH" if confirmed > 0 else "MEDIUM"

    return Finding(
        "osint-registry",
        "The identity_links table holds resolved cross-registry entity matches.",
        verdict, severity,
        f"{total} identity link(s): {confirmed} confirmed/auto-linked, "
        f"{pending} pending review, {candidate} candidates. "
        f"Avg confidence {avg_conf:.3f}.",
        evidence=ev, confidence="high", tier="osint",
    )


def probe_osint_confirmed_intel(db: SurrealClient, st: Strategy) -> Finding:
    """Confirmed and auto-linked identity resolutions — direct actionable intelligence."""
    # Both branches report len() as the finding, so neither may carry a ceiling. The
    # cheap-path/full-path split is kept only because the paged read stops on the first
    # empty page when the table really is empty.
    confirmed_links = [r for r in fetch_identity_links(db)
                       if r.get("status") in ("confirmed", "auto_linked")]

    if not confirmed_links:
        return Finding(
            "osint-confirmed-intel",
            "Confirmed identity matches cross-reference procurement applicants to risk registries.",
            "DATA_GAP", "MEDIUM",
            "No confirmed or auto-linked identity resolutions exist yet. "
            "Once Phase 17+ adapters run, confirmed links will appear here — e.g. 'planning applicant "
            "J. Smith confirmed as director of 5 dissolved companies'.",
            confidence="high", tier="osint",
        )

    # Group by resolution_type for framing
    by_type: dict = defaultdict(list)
    for r in confirmed_links:
        by_type[r.get("resolution_type", "unknown")].append(r)

    ev = []
    for rtype, items in sorted(by_type.items(), key=lambda kv: -len(kv[1])):
        ev.append(f"  {rtype} ({len(items)} link(s)):")
        for item in items[:5]:
            src = _short_node_id(item.get("source_node_id", ""))
            dst = _short_node_id(item.get("target_node_id", ""))
            conf = item.get("confidence", 0)
            bar = _confidence_bar(conf)
            adapter = item.get("source", "?")
            ev.append(f"    [{bar}] {conf:.3f}  {src}  →  {dst}  [{adapter}]")
            # Surface any structured evidence
            evd = _parse_evidence(item.get("evidence_json", ""))
            for key in ("reason", "match_basis", "ch_number", "dissolution_date", "disqualification_end"):
                if key in evd:
                    ev.append(f"      {key}: {evd[key]}")
        if len(items) > 5:
            ev.append(f"    ... and {len(items) - 5} more")

    # Flag particularly high-risk resolution types
    high_risk_types = {"disqualified_director", "phoenix_officer", "psc_beneficial_owner",
                       "offshore_entity", "sanctions_hit"}
    risky = [r for r in confirmed_links if r.get("resolution_type", "") in high_risk_types]

    severity = "CRITICAL" if risky else "HIGH"
    summary = (
        f"{len(confirmed_links)} confirmed/auto-linked resolution(s) across "
        f"{len(by_type)} type(s)."
    )
    if risky:
        summary += f" {len(risky)} HIGH-RISK resolution(s) found ({', '.join(set(r.get('resolution_type','') for r in risky))})."

    return Finding(
        "osint-confirmed-intel",
        "Confirmed identity matches cross-reference procurement applicants to risk registries.",
        "HIT", severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_analyst_review_queue(db: SurrealClient, st: Strategy) -> Finding:
    """Pending review queue — identity links at 0.70–0.84 confidence needing analyst sign-off."""
    pending = fetch_identity_links(db, status_filter="pending_review")  # queue size is the finding

    if not pending:
        if not st.osint_active:
            return Finding(
                "osint-analyst-queue",
                "Analyst review queue: pending_review links at 0.70–0.84 confidence.",
                "DATA_GAP", "INFO",
                "No pending_review links. Either OSINT enrichment has not run yet, "
                "or all links are outside the 0.70–0.84 review window.",
                confidence="high", tier="osint",
            )
        return Finding(
            "osint-analyst-queue",
            "Analyst review queue: pending_review links at 0.70–0.84 confidence.",
            "CLEAR", "INFO",
            "No links currently need analyst review.",
            confidence="high", tier="osint",
        )

    # Sort by confidence desc (highest-confidence first — easiest to sign off)
    pending.sort(key=lambda r: r.get("confidence", 0), reverse=True)

    by_adapter: Counter = Counter(r.get("source", "unknown") for r in pending)
    by_type: Counter = Counter(r.get("resolution_type", "unknown") for r in pending)

    ev = [
        f"  {len(pending)} link(s) pending review  (use `urban-integrity links list` to manage)",
        f"",
        f"  Top 10 by confidence (confirm with `urban-integrity links confirm <id>`):",
    ]
    for r in pending[:10]:
        src = _short_node_id(r.get("source_node_id", ""))
        dst = _short_node_id(r.get("target_node_id", ""))
        conf = r.get("confidence", 0)
        bar = _confidence_bar(conf)
        link_id = str(r.get("id", ""))
        ev.append(f"    [{bar}] {conf:.3f}  {src}  →  {dst}")
        ev.append(f"      id: {link_id}")
        evd = _parse_evidence(r.get("evidence_json", ""))
        if evd.get("match_basis"):
            ev.append(f"      match_basis: {evd['match_basis']}")

    if len(pending) > 10:
        ev.append(f"  ... and {len(pending) - 10} more")

    ev += ["", "  By adapter:"] + [f"    {n:>4}  {a}" for a, n in by_adapter.most_common(5)]
    ev += ["", "  By resolution type:"] + [f"    {n:>4}  {t}" for t, n in by_type.most_common(5)]

    return Finding(
        "osint-analyst-queue",
        "Analyst review queue: pending_review links at 0.70–0.84 confidence.",
        "WEAK", "MEDIUM",
        f"{len(pending)} link(s) pending analyst sign-off. "
        f"Unreviewed links are excluded from hot-graph traversal — confirm or reject to activate.",
        evidence=ev, confidence="high", tier="osint",
    )


def probe_property_nodes(db: SurrealClient, st: Strategy) -> Finding:
    """Land Registry CCOD: Property nodes in the graph from bulk title data.

    Always returns DATA_GAP until CCOD runs — documents that property data is
    absent due to missing API key rather than silently omitting the dimension.
    """
    rows = db.query(
        "SELECT count() AS n FROM graph_events "
        "WHERE kind = 'NodeAdded' AND payload.weight_json CONTAINS '\"type\":\"Property\"' "
        "GROUP ALL;"
    )
    count = rows[0]["n"] if rows else 0

    if count == 0:
        return Finding(
            "property-nodes",
            "Land Registry CCOD: corporate property ownership in the graph.",
            "DATA_GAP", "LOW",
            "Zero Property nodes. Land Registry CCOD adapter is registered but disabled "
            "(URBAN__ENRICHER__LAND_REGISTRY__API_KEY not set). "
            "Property accumulation anomaly detection requires this data.",
            evidence=[
                "CCOD adapter status: registered, is_enabled() = false",
                "Reason: api_key absent — set URBAN__ENRICHER__LAND_REGISTRY__API_KEY",
                "Dataset: HM Land Registry 'Use Land and Property Data' (CCOD bulk CSV)",
                "URL: https://use-land-property-data.service.gov.uk/datasets/ccod",
            ],
            confidence="high", tier="osint",
        )

    return Finding(
        "property-nodes",
        "Land Registry CCOD: corporate property ownership in the graph.",
        "HIT", "HIGH",
        f"{count} Property node(s) in graph from Land Registry CCOD data.",
        evidence=[f"  {count}  Property nodes (corporate title ownership)"],
        confidence="high", tier="osint",
    )


def probe_osint_new_node_types(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 17+ OSINT node type coverage: PSC, Charity, Property, OffshoreEntity."""
    counts = {}
    for t in ("Psc", "Charity", "Property", "OffshoreEntity"):
        rows = db.query(
            f"SELECT count() AS n FROM graph_events WHERE kind='NodeAdded' "
            f"AND payload.weight_json CONTAINS '\"type\":\"{t}\"' GROUP ALL;"
        )
        counts[t] = rows[0]["n"] if rows else 0

    # InLiquidation comes from NodeUpdated events (Gazette adapter), not NodeAdded
    inliq_rows = db.query(
        "SELECT count() AS n FROM graph_events "
        "WHERE kind = 'NodeUpdated' AND payload.weight_json CONTAINS 'InLiquidation' GROUP ALL;"
    )
    inliq_count = inliq_rows[0]["n"] if inliq_rows else 0

    total_osint_nodes = sum(counts.values())
    ev = [
        f"OSINT node type coverage:",
        f"  {counts['Psc']:>6}  Psc (Persons with Significant Control — Phase 17)",
        f"  {counts['Charity']:>6}  Charity (Charity Commission — Phase 19)",
        f"  {counts['Property']:>6}  Property (Land Registry CCOD — Phase 18)",
        f"  {counts['OffshoreEntity']:>6}  OffshoreEntity (ICIJ Offshore Leaks — Phase 19)",
        f"  {inliq_count:>6}  InLiquidation status updates (London Gazette — Phase 18)",
    ]

    if total_osint_nodes == 0:
        ev.append("")
        if osint_enrichment_ran(db):
            ev.append(f"  {_CLI_DROPS_MUTATION_NOTE}")
            return Finding(
                "osint-node-coverage",
                "OSINT-sourced node types (PSC, Charity, Property, Offshore) are present in the graph.",
                "DATA_GAP", "MEDIUM",
                "Zero OSINT-sourced nodes in the graph, but enrichment HAS run "
                "(run markers and/or identity links exist). "
                + _CLI_DROPS_MUTATION_NOTE,
                evidence=ev, confidence="high", tier="osint",
            )
        ev.append("  Run `urban-integrity enrich --source all` after wiring Phase 17–19 adapters.")
        return Finding(
            "osint-node-coverage",
            "OSINT-sourced node types (PSC, Charity, Property, Offshore) are present in the graph.",
            "DATA_GAP", "MEDIUM",
            "Zero OSINT-sourced nodes in the graph. Phase 17–19 adapters have not yet run. "
            "These adapters unlock beneficial ownership, land accumulation, and charity-commercial "
            "crossover detection.",
            evidence=ev, confidence="high", tier="osint",
        )

    present = [t for t, n in counts.items() if n > 0]
    # Offshore is the highest-risk type
    if counts["OffshoreEntity"] > 0:
        severity = "CRITICAL"
        verdict = "HIT"
        summary = (f"{total_osint_nodes} OSINT nodes. "
                   f"OFFSHORE ENTITIES PRESENT ({counts['OffshoreEntity']}) — "
                   "requires immediate cross-reference with contract awardees.")
    elif counts["Psc"] > 0:
        severity = "HIGH"
        verdict = "HIT"
        summary = (f"{total_osint_nodes} OSINT nodes including {counts['Psc']} PSC records — "
                   "beneficial ownership graph active. Types: " + ", ".join(present))
    else:
        severity = "MEDIUM"
        verdict = "WEAK"
        summary = (f"{total_osint_nodes} OSINT nodes present. Types: " + ", ".join(present))

    return Finding(
        "osint-node-coverage",
        "OSINT-sourced node types (PSC, Charity, Property, Offshore) are present in the graph.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_disqualified_director_edges(db: SurrealClient, st: Strategy) -> Finding:
    """DisqualifiedFrom and Controls edges — the sharpest Phoenix-evasion signal."""
    disq_edges = fetch_edges(db, "DisqualifiedFrom")
    controls_edges = fetch_edges(db, "Controls")
    offshore_edges = fetch_edges(db, "OffshoreLink")
    donation_edges = fetch_edges(db, "DonatesTo")
    trustee_edges = fetch_edges(db, "TrusteeOf")

    total_risk_edges = len(disq_edges) + len(controls_edges) + len(offshore_edges)

    ev = [
        f"OSINT-sourced edge type coverage:",
        f"  {len(disq_edges):>6}  DisqualifiedFrom  — individuals barred from directorships (CRITICAL risk)",
        f"  {len(controls_edges):>6}  Controls          — beneficial ownership / PSC chains (HIGH risk)",
        f"  {len(offshore_edges):>6}  OffshoreLink      — connections to offshore jurisdictions (HIGH risk)",
        f"  {len(donation_edges):>6}  DonatesTo         — political donation bridges (MEDIUM risk)",
        f"  {len(trustee_edges):>6}  TrusteeOf         — charity trustee relationships (MEDIUM risk)",
    ]

    if total_risk_edges == 0 and not disq_edges:
        ev.append("")
        if osint_enrichment_ran(db):
            ev.append(f"  {_CLI_DROPS_MUTATION_NOTE}")
            return Finding(
                "osint-risk-edges",
                "DisqualifiedFrom, Controls, OffshoreLink edges expose high-risk entity relationships.",
                "DATA_GAP", "MEDIUM",
                "No OSINT-sourced risk edges in the graph, but enrichment HAS run "
                "(run markers and/or identity links exist). "
                + _CLI_DROPS_MUTATION_NOTE,
                evidence=ev, confidence="high", tier="osint",
            )
        ev.append("  These edges populate once Phase 17–20 adapters run.")
        return Finding(
            "osint-risk-edges",
            "DisqualifiedFrom, Controls, OffshoreLink edges expose high-risk entity relationships.",
            "DATA_GAP", "MEDIUM",
            "No OSINT-sourced risk edges in the graph. Phase 17+ adapters (CH PSC, "
            "Disqualified Directors, ICIJ) have not run yet.",
            evidence=ev, confidence="high", tier="osint",
        )

    if disq_edges:
        # Show the disqualified director nodes — these are the most actionable
        disq_directors = set(e["src"] for e in disq_edges if e.get("src"))
        ev.append("")
        ev.append(f"  Disqualified director nodes ({len(disq_directors)} unique):")
        for d in list(disq_directors)[:8]:
            ev.append(f"    {_short_node_id(d)}")

    severity = "CRITICAL" if disq_edges else "HIGH"
    verdict = "HIT"
    summary = (
        f"{len(disq_edges)} DisqualifiedFrom edge(s), "
        f"{len(controls_edges)} Controls, "
        f"{len(offshore_edges)} OffshoreLink. "
        + ("DISQUALIFIED DIRECTORS ACTIVE IN PROCUREMENT — cross-reference contract awards immediately."
           if disq_edges else "Beneficial ownership and offshore structure data present.")
    )

    return Finding(
        "osint-risk-edges",
        "DisqualifiedFrom, Controls, OffshoreLink edges expose high-risk entity relationships.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_psc_beneficial_ownership(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 17 — PSC adapter: who really controls the procurement applicants?

    Queries identity_links sourced from ch_psc and cross-references source_node_id
    (the PSC / Psc node) with known Entity nodes that have contract awards, surfacing
    beneficial owners who appear to control companies winning public contracts.
    """
    psc_links = fetch_identity_links(db, source_filter="ch_psc")  # count is the finding — read all

    if not psc_links:
        marker = fetch_enrichment_runs(db).get("ch_psc")
        if marker:
            # Ran, zero graph overlap — a data-coverage observation, not an action item.
            cf = marker.get("candidates_found", 0)
            day = _ts_to_day(marker.get("last_run_at"))
            return Finding(
                "osint-psc-ownership",
                "PSC (Persons with Significant Control) records expose true beneficial owners.",
                "CLEAR", "INFO",
                f"ch_psc ran {day} — 0 graph overlap (candidates_found={cf}, 0 linked). "
                "The PSC adapter executed; the Companies House PSC register simply does "
                "not intersect any entity currently in the graph. No action required.",
                confidence="high", tier="osint",
            )
        return Finding(
            "osint-psc-ownership",
            "PSC (Persons with Significant Control) records expose true beneficial owners.",
            "DATA_GAP", "HIGH",
            "No ch_psc identity links found. Run `urban-integrity enrich --source psc` "
            "with URBAN__ENRICHER__CH__API_KEY set to populate beneficial ownership chains.",
            confidence="high", tier="osint",
        )

    confirmed = [r for r in psc_links if r.get("status") in ("auto_linked", "confirmed")]
    pending   = [r for r in psc_links if r.get("status") == "pending_review"]

    # Count unique officer targets with PSC links
    officer_targets: set = {r.get("target_node_id", "") for r in confirmed if r.get("target_node_id")}

    # Sample evidence — top confirmed links
    ev = [
        f"Beneficial ownership (PSC) summary:",
        f"  {len(psc_links):>5}  total PSC identity link(s)",
        f"  {len(confirmed):>5}  confirmed / auto-linked  (score ≥ 0.85)",
        f"  {len(pending):>5}  pending analyst review   (0.70–0.84)",
        f"  {len(officer_targets):>5}  unique officer/director nodes with PSC confirmation",
        "",
        f"  Sample confirmed PSC → officer links:",
    ]
    for r in confirmed[:8]:
        src = _short_node_id(r.get("source_node_id", ""))
        dst = _short_node_id(r.get("target_node_id", ""))
        conf = r.get("confidence", 0.0)
        evd = _parse_evidence(r.get("evidence_json", ""))
        ch_num = evd.get("ch_number", evd.get("company_number", ""))
        natures = evd.get("natures_of_control", [])
        nature_str = ", ".join(natures[:2]) if isinstance(natures, list) else str(natures)
        line = f"    {conf:.3f}  {src}  →  {dst}"
        if ch_num:
            line += f"  [CH: {ch_num}]"
        if nature_str:
            line += f"  ({nature_str})"
        ev.append(line)
    if len(confirmed) > 8:
        ev.append(f"    ... and {len(confirmed) - 8} more")

    severity = "HIGH" if confirmed else "MEDIUM"
    verdict = "HIT" if confirmed else ("WEAK" if pending else "CLEAR")
    summary = (
        f"{len(confirmed)} confirmed beneficial-ownership link(s) across "
        f"{len(officer_targets)} officer node(s). "
        + (f"{len(pending)} pending review." if pending else "")
    )
    return Finding(
        "osint-psc-ownership",
        "PSC (Persons with Significant Control) records expose true beneficial owners.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_gazette_insolvency_signals(db: SurrealClient, st: Strategy) -> Finding:
    """London Gazette insolvency adapter: entities in liquidation that hold live contracts.

    Queries NodeUpdated events for InLiquidation status set by the Gazette adapter,
    then intersects with AwardedTo edges to find companies that won procurement
    contracts and are now in liquidation — the early-Phoenix precursor signal.
    """
    # Step 1: entities whose status was updated to InLiquidation by the Gazette adapter
    # CR-02: row-returning, so `kind` stays out of the WHERE clause and is filtered here.
    inliq_rows = [
        r
        for r in db.query(
            "SELECT payload.entity_id AS entity_id, kind FROM graph_events "
            "WHERE payload.weight_json CONTAINS 'InLiquidation' LIMIT 500;"
        )
        if r.get("kind") == "NodeUpdated"
    ]

    if not inliq_rows:
        return Finding(
            "gazette-insolvency-signals",
            "London Gazette insolvency notices: companies in liquidation holding live contracts.",
            "DATA_GAP", "MEDIUM",
            "No NodeUpdated events with InLiquidation status found. "
            "Run `./scripts/enrich.sh --source london_gazette` to poll the Gazette API.",
            confidence="high", tier="osint",
        )

    inliq_entities: set = {r["entity_id"] for r in inliq_rows if r.get("entity_id")}

    # Step 2: entities that appear as the source of an AwardedTo edge (contract holders)
    # CR-02: was `kind = 'EdgeAdded' AND ... CONTAINS 'AwardedTo' LIMIT 5000`, which had
    # both defects at once — the banned planner shape, and a fixed limit roughly an order
    # of magnitude below the live AwardedTo edge count (~40k on an_anglesey_aebaa785), so
    # the intersection below was computed against a fraction of contract holders and
    # under-reported insolvent ones. fetch_edges already paginates by rows-received and
    # filters `kind` in Python.
    awarded_entities: set = {
        e["src"] for e in fetch_edges(db, "AwardedTo") if e.get("src")
    }

    # Intersection: in liquidation AND has contracts
    flagged = sorted(inliq_entities & awarded_entities)

    ev = [
        f"Entities with InLiquidation status (from Gazette): {len(inliq_entities)}",
        f"Entities with AwardedTo edges (contract holders):   {len(awarded_entities)}",
        f"Intersection — in liquidation AND holds contracts:  {len(flagged)}",
    ]
    if flagged:
        ev.append("")
        ev.append("Flagged entities (in liquidation + live contracts):")
        for eid in flagged[:20]:
            ev.append(f"  {_short_node_id(eid)}")
        if len(flagged) > 20:
            ev.append(f"  ... and {len(flagged) - 20} more")
    else:
        ev.append("")
        ev.append(f"  {len(inliq_entities)} entities marked InLiquidation by Gazette,")
        ev.append("  but none intersect with contract-award records.")
        ev.append("  This is expected if Gazette-matched companies have not been awarded")
        ev.append("  contracts in the Contracts Finder dataset.")

    if flagged:
        verdict, severity = "HIT", "CRITICAL"
        summary = (
            f"{len(flagged)} entity/ies in liquidation hold live procurement contracts — "
            "early Phoenix loop precursor signal. Cross-reference with contract award dates."
        )
    else:
        verdict, severity = "CLEAR", "MEDIUM"
        summary = (
            f"Gazette adapter active: {len(inliq_entities)} InLiquidation event(s) found. "
            "None intersect with contract-award records in this dataset."
        )

    return Finding(
        "gazette-insolvency-signals",
        "London Gazette insolvency notices: companies in liquidation holding live contracts.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def _disqualification_chain(db: SurrealClient) -> dict:
    """Measure where the disqualified-director pipeline actually stops.

    Two probes used to report a *disjunction* here — "either no links exist, or nobody is
    inside their ban window" — which reads as though both were equally likely and neither
    was checked. They are cheap to distinguish, and the answer changes what to do about it.

    The chain, in order, with the code that breaks each link:

      1. Officer nodes                      ch_disqualified.rs:551 (`fetch_officer_names_from_graph`)
      2. CH disqualified-officers REST call  ch_disqualified.rs:562 (loop over those officers)
      3. identity_links (source=ch_disqualified)
      4. status IN [auto_linked, confirmed]  evasion.rs:64-80 (the ENR-11 gate)
      5. officer Active AND inside ban window evasion.rs:107-139

    Step 1 is the one that bites today, and it fails silently in the worst way: the adapter
    iterates over the officer list, so an empty list means the loop body never runs, no CH
    request is ever issued, and the run marker records `candidates_found=0` — indistinguishable,
    from the marker alone, from "the register was searched and matched nobody".

    Returns a dict with the measured counts, a `cause` slug, and a prose `explain`.
    """
    officer_nodes = _count_motif_term(
        db, ("NodeAdded", ["payload.weight_json CONTAINS '\"type\":\"Officer\"'"])
    )
    # Directors ARE in the graph — as Entity nodes keyed `director:{slug}` (a load-bearing
    # dedup invariant, see docs/ARCHITECTURE.md). They are simply not the node type every officer-consuming
    # path queries for, which is why "no Officer nodes" is a typing gap, not a data gap.
    # `type::string` because SurrealDB 3.x hands back `payload.entity_id` as a RecordId for
    # `director:*` ids, and `string::starts_with` on a RecordId does not match.
    director_entities = _count_motif_term(
        db,
        (
            "NodeAdded",
            [
                "payload.weight_json CONTAINS '\"type\":\"Entity\"'",
                "string::starts_with(type::string(payload.entity_id), 'director:')",
            ],
        ),
    )
    disq_edges = _count_motif_term(
        db, ("EdgeAdded", ["payload.weight_json CONTAINS '\"type\":\"DisqualifiedFrom\"'"])
    )
    links = fetch_identity_links(db, source_filter="ch_disqualified")  # count is the finding
    elevated = [r for r in links if r.get("status") in ("auto_linked", "confirmed")]
    marker = fetch_enrichment_runs(db).get("ch_disqualified")
    day = _ts_to_day(marker.get("last_run_at")) if marker else None

    state = {
        "officer_nodes": officer_nodes,
        "director_entities": director_entities,
        "disq_edges": disq_edges,
        "links": links,
        "elevated": elevated,
        "marker": marker,
        "day": day,
    }

    if officer_nodes == 0:
        state["cause"] = "no-officer-nodes"
        ran = f"The adapter last ran {day}" if day else "The adapter has not run"
        state["explain"] = (
            f"Measured cause: the graph holds 0 Officer nodes, so the adapter's input set is "
            f"empty and it issued zero Companies House requests. "
            f"{ran}; its marker reads candidates_found=0 because the counter lives inside the "
            f"per-officer loop, which never executed "
            f"(crates/enricher/src/adapters/ch_disqualified.rs:551-562). "
            f"The register was never searched — this is not evidence that it fails to overlap. "
            f"Directors are present as {director_entities:,} Entity nodes keyed `director:{{slug}}`, "
            f"but every officer-consuming path filters on `\"type\":\"Officer\"`. "
            f"Re-running `enrich` cannot change this; Phase 26 (PHX-01–05) is what creates "
            f"Officer nodes."
        )
    elif marker is None:
        state["cause"] = "adapter-never-ran"
        state["explain"] = (
            f"Measured cause: {officer_nodes:,} Officer nodes exist, but the ch_disqualified "
            f"adapter has no run marker — it has never executed. Set "
            f"URBAN__ENRICHER__CH__API_KEY (the REST key) and run "
            f"`urban-integrity enrich --source psc`."
        )
    elif not links:
        state["cause"] = "no-register-overlap"
        cf = marker.get("candidates_found", 0)
        state["explain"] = (
            f"Measured cause: the adapter ran {day} against {officer_nodes:,} Officer nodes and "
            f"searched the register (candidates_found={cf:,}), but no result met the "
            f"three-field minimum (name + at least birth year), so no link was written. "
            f"The register genuinely does not intersect this graph."
        )
    elif not elevated:
        state["cause"] = "links-not-elevated"
        state["explain"] = (
            f"Measured cause: {len(links)} ch_disqualified link(s) exist but none reached "
            f"`auto_linked`/`confirmed`. The ENR-11 status gate "
            f"(crates/enricher/src/evasion.rs:64-80) deliberately keeps sub-threshold "
            f"candidates away from a CRITICAL alert. Review them with "
            f"`urban-integrity links --status pending_review`."
        )
    else:
        state["cause"] = "no-live-ban"
        state["explain"] = (
            f"Measured cause: {len(elevated)} confirmed disqualified-director link(s) exist and "
            f"the detector evaluated them, but none is both currently Active and inside its ban "
            f"window ({disq_edges:,} DisqualifiedFrom edge(s) carry the windows). This is a "
            f"genuine clean result, not a coverage gap."
        )
    return state


def probe_disqualified_active_in_procurement(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 17 — Disqualified Directors adapter: banned directors directing active companies.

    Queries identity_links for confirmed ch_disqualified entries, then cross-references
    the linked officer nodes against AwardedTo and IssuedBy edges to check whether any
    disqualified director's associated entity is still winning public contracts.
    """
    disq_links = fetch_identity_links(db, source_filter="ch_disqualified")  # count is the finding

    if not disq_links:
        # This used to report a bare CLEAR whenever a run marker existed — "the register simply
        # does not intersect any director currently in the graph". That is a claim about the
        # register, and with 0 Officer nodes it was never tested: no request was sent. Report
        # the measured break in the chain instead.
        chain = _disqualification_chain(db)
        verdict, severity = (
            ("CLEAR", "INFO")
            if chain["cause"] == "no-register-overlap"
            else ("DATA_GAP", "HIGH")
        )
        return Finding(
            "osint-disqualified-directors",
            "Disqualified Directors register: banned individuals directing procurement applicants.",
            verdict, severity,
            f"No ch_disqualified identity links found. {chain['explain']}",
            confidence="high", tier="osint",
        )

    confirmed = [r for r in disq_links if r.get("status") in ("auto_linked", "confirmed")]
    pending   = [r for r in disq_links if r.get("status") == "pending_review"]

    # The officer node in the identity link is target_node_id (director:slug).
    # We want to know: does this director slug appear as the src of any AwardedTo edge?
    officer_ids: list[str] = [
        r.get("target_node_id", "") for r in confirmed if r.get("target_node_id")
    ]

    # Fetch AwardedTo edges to see which entities are winning contracts
    award_edges = fetch_edges(db, "AwardedTo", limit=20000)
    award_srcs: set = {e["src"] for e in award_edges if e.get("src")}

    # Cross-reference: any disqualified officer directing a company with awards?
    active_and_winning: list[dict] = []
    for r in confirmed:
        officer = r.get("target_node_id", "")
        evd = _parse_evidence(r.get("evidence_json", ""))
        ch_num = evd.get("ch_number", "")
        entity_id = f"GB-COH:{ch_num}" if ch_num else None
        if entity_id and entity_id in award_srcs:
            active_and_winning.append({**r, "_entity_id": entity_id})
        elif officer in award_srcs:
            active_and_winning.append({**r, "_entity_id": officer})

    ev = [
        f"Disqualified Director summary:",
        f"  {len(disq_links):>5}  total disqualified director link(s)",
        f"  {len(confirmed):>5}  confirmed / auto-linked",
        f"  {len(pending):>5}  pending analyst review",
        f"  {len(active_and_winning):>5}  confirmed disqualified directors whose entity has contract awards",
        "",
    ]

    if confirmed:
        ev.append("  Confirmed disqualified directors:")
        for r in confirmed[:8]:
            dst = _short_node_id(r.get("target_node_id", ""))
            src = _short_node_id(r.get("source_node_id", ""))
            evd = _parse_evidence(r.get("evidence_json", ""))
            disq_until = evd.get("disqualified_until", evd.get("disqualification_end", "?"))
            case_num = evd.get("case_number", "")
            line = f"    {dst}  (disq: {disq_until}"
            if case_num:
                line += f", case: {case_num}"
            line += ")"
            ev.append(line)
        if len(confirmed) > 8:
            ev.append(f"    ... and {len(confirmed) - 8} more")

    if active_and_winning:
        ev.append("")
        ev.append("  ⚠ Confirmed disqualified directors linked to contract-award entities:")
        for r in active_and_winning[:5]:
            ev.append(f"    {_short_node_id(r.get('target_node_id', ''))} → "
                      f"{r.get('_entity_id', '?')}")

    if active_and_winning:
        severity = "CRITICAL"
        verdict = "HIT"
        summary = (
            f"{len(confirmed)} confirmed disqualified director(s); "
            f"{len(active_and_winning)} linked to entities with contract awards — "
            "potential CDDA 1986 violation."
        )
    elif confirmed:
        severity = "HIGH"
        verdict = "HIT"
        summary = (
            f"{len(confirmed)} confirmed disqualified director(s). "
            "None cross-referenced to active contract awards yet."
        )
    elif pending:
        severity = "MEDIUM"
        verdict = "WEAK"
        summary = f"{len(pending)} disqualified director link(s) pending analyst review."
    else:
        severity = "MEDIUM"
        verdict = "CLEAR"
        summary = "No confirmed disqualified directors found in identity_links."

    return Finding(
        "osint-disqualified-directors",
        "Disqualified Directors register: banned individuals directing procurement applicants.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_electoral_commission_donations(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 20 POL-01 — Electoral Commission donations adapter.

    Queries identity_links for links sourced from the EC CSV adapter and
    cross-references with DonatesTo graph edges to surface political donors
    who are also procurement applicants or their controllers.
    """
    ec_links = fetch_identity_links(db, source_filter="electoral_commission")  # count is the finding

    # Count DonatesTo edges written to graph_events
    donates_rows = db.query(
        "SELECT count() AS n FROM graph_events "
        "WHERE kind = 'EdgeAdded' AND payload.weight_json CONTAINS 'DonatesTo' GROUP ALL;"
    )
    donates_count = donates_rows[0]["n"] if donates_rows else 0

    if not ec_links and donates_count == 0:
        marker = fetch_enrichment_runs(db).get("electoral_commission")
        if marker:
            # Ran, zero graph overlap — a data-coverage observation, not an action item.
            cf = marker.get("candidates_found", 0)
            day = _ts_to_day(marker.get("last_run_at"))
            return Finding(
                "phase20-ec-donations",
                "Electoral Commission donations: political donors cross-referenced with procurement applicants.",
                "CLEAR", "INFO",
                f"electoral_commission ran {day} — 0 graph overlap "
                f"(candidates_found={cf}, 0 linked, 0 DonatesTo edges). The EC donations "
                "adapter executed; no donor in the EC register intersects an entity "
                "currently in the graph. No action required.",
                evidence=[
                    f"EC run marker: last_run_at={day}, candidates_found={cf}, "
                    f"auto_linked={marker.get('auto_linked', 0)}, "
                    f"pending_review={marker.get('pending_review', 0)}, "
                    f"discarded={marker.get('discarded', 0)}",
                    "Resolution type: company_donor only — exact CH company_number join, "
                    "confidence 1.0 (individual-donor matching disabled, exact-match-only policy)",
                ],
                confidence="high", tier="osint",
            )
        return Finding(
            "phase20-ec-donations",
            "Electoral Commission donations: political donors cross-referenced with procurement applicants.",
            "DATA_GAP", "MEDIUM",
            "No Electoral Commission identity links or DonatesTo edges found. "
            "Run `urban-integrity enrich --source electoral` to download the EC donations CSV. "
            "Adapter is enabled by default (OGL v3 open data, no API key required).",
            evidence=[
                "EC adapter status: registered, is_enabled() = true",
                "Requires network access to EC bulk data download endpoint",
                "Resolution type: company_donor only — exact CH company_number join, "
                "confidence 1.0 (individual-donor matching disabled, exact-match-only policy)",
            ],
            confidence="high", tier="osint",
        )

    confirmed = [r for r in ec_links if r.get("status") in ("auto_linked", "confirmed")]
    pending   = [r for r in ec_links if r.get("status") == "pending_review"]
    by_type: Counter = Counter(r.get("resolution_type", "unknown") for r in ec_links)

    ev = [
        f"Electoral Commission donations summary:",
        f"  {len(ec_links):>5}  total EC identity link(s)",
        f"  {len(confirmed):>5}  confirmed / auto-linked  (company_number exact match, confidence 1.0)",
        f"  {len(pending):>5}  pending analyst review   (ambiguous CH number)",
        f"  {donates_count:>5}  DonatesTo edges in graph",
        "",
        "  Resolution type breakdown:",
    ]
    for rt, n in by_type.most_common(5):
        ev.append(f"    {n:>4}  {rt}")

    if confirmed:
        ev.append("")
        ev.append("  Top confirmed donor → entity links:")
        for r in confirmed[:6]:
            src = _short_node_id(r.get("source_node_id", ""))
            dst = _short_node_id(r.get("target_node_id", ""))
            conf = r.get("confidence", 0.0)
            evd = _parse_evidence(r.get("evidence_json", ""))
            party = evd.get("party_name", "")
            amount = evd.get("donation_amount", "")
            line = f"    {conf:.3f}  {src}  →  {dst}"
            if party:
                line += f"  [{party}]"
            if amount:
                line += f"  £{amount}"
            ev.append(line)
        if len(confirmed) > 6:
            ev.append(f"    ... and {len(confirmed) - 6} more")

    verdict = "HIT" if confirmed else ("WEAK" if pending or donates_count > 0 else "DATA_GAP")
    severity = "HIGH" if confirmed else "MEDIUM"
    summary = (
        f"{len(ec_links)} EC donation link(s): {len(confirmed)} confirmed, {len(pending)} pending. "
        f"{donates_count} DonatesTo edge(s) in graph."
    )
    return Finding(
        "phase20-ec-donations",
        "Electoral Commission donations: political donors cross-referenced with procurement applicants.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_council_registers_political_actors(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 20 POL-02 — Council Registers of Interest adapter.

    Checks for PoliticalActor nodes from the Scrapling sidecar councillor
    declaration scraper and Directs edges linking councillors to entities.
    These edges are the primary input to the nepotism bridge detector.
    """
    # PoliticalActor nodes (any source — council_registers or ingestor)
    pa_rows = db.query(
        "SELECT count() AS n FROM graph_events "
        "WHERE kind = 'NodeAdded' AND payload.weight_json CONTAINS '\"type\":\"PoliticalActor\"' "
        "GROUP ALL;"
    )
    pa_count = pa_rows[0]["n"] if pa_rows else 0

    # Directs edges (councillor → company)
    directs_rows = db.query(
        "SELECT count() AS n FROM graph_events "
        "WHERE kind = 'EdgeAdded' AND payload.weight_json CONTAINS 'Directs' GROUP ALL;"
    )
    directs_count = directs_rows[0]["n"] if directs_rows else 0

    # Sample PoliticalActor nodes with party/council_name fields (Phase 20 extension)
    # CR-02: row-returning, so `kind` is filtered in Python, not in the WHERE clause.
    # Over-fetch modestly so the kind filter still leaves a usable sample.
    pa_sample = [
        r
        for r in db.query(
            "SELECT payload.weight_json AS w, kind FROM graph_events "
            "WHERE payload.weight_json CONTAINS '\"type\":\"PoliticalActor\"' "
            "LIMIT 100;"
        )
        if r.get("kind") == "NodeAdded"
    ][:20]
    with_party = 0
    with_council = 0
    for r in pa_sample:
        wj = r.get("w", "{}")
        d = _parse_evidence(wj) if isinstance(wj, str) else wj
        data = d.get("data", d)
        if isinstance(data, dict):
            if data.get("party"):
                with_party += 1
            if data.get("council_name"):
                with_council += 1

    ev = [
        f"Council Registers / PoliticalActor summary:",
        f"  {pa_count:>6}  PoliticalActor nodes in graph",
        f"  {directs_count:>6}  Directs edges (councillor → company)",
        f"  {with_party:>6}  PoliticalActor nodes with party affiliation (Phase 20 field)",
        f"  {with_council:>6}  PoliticalActor nodes with council_name (Phase 20 field)",
    ]

    if pa_count == 0:
        ev.append("")
        ev.append("  Council Registers adapter requires Scrapling sidecar (localhost:8765)")
        ev.append("  and LPA codes configured in [enricher.council_registers].")
        ev.append("  Run `urban-integrity enrich --source council_registers` after configuring.")
        return Finding(
            "phase20-council-registers",
            "Council Registers: councillor declarations of interest linking political actors to companies.",
            "DATA_GAP", "MEDIUM",
            "No PoliticalActor nodes found. Council Registers adapter requires Scrapling sidecar "
            "and LPA codes to be configured. Nepotism bridge detection is data-gapped.",
            evidence=ev, confidence="high", tier="osint",
        )

    # Are Directs edges present? These are needed for nepotism detection.
    if directs_count == 0:
        verdict, severity = "WEAK", "MEDIUM"
        summary = (f"{pa_count} PoliticalActor node(s) present but 0 Directs edges — "
                   "councillor–company links absent, nepotism detector cannot fire.")
    else:
        verdict, severity = "HIT", "HIGH"
        summary = (f"{pa_count} PoliticalActor node(s) and {directs_count} Directs edge(s) — "
                   "nepotism bridge detector inputs active.")

    return Finding(
        "phase20-council-registers",
        "Council Registers: councillor declarations of interest linking political actors to companies.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_opencorporates_cross_jurisdiction(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 20 POL-03 — OpenCorporates cross-jurisdiction adapter.

    Queries identity_links for OC-sourced cross-jurisdiction company matches
    and checks the oc_quota table for monthly API usage.
    """
    oc_links = fetch_identity_links(db, source_filter="opencorporates")  # count is the finding

    # Check oc_quota table for usage this month
    try:
        quota_rows = db.query(
            "SELECT year_month, request_count FROM oc_quota ORDER BY year_month DESC LIMIT 3;"
        )
    except Exception:
        quota_rows = []

    confirmed = [r for r in oc_links if r.get("status") in ("auto_linked", "confirmed")]
    pending   = [r for r in oc_links if r.get("status") == "pending_review"]

    ev = [
        f"OpenCorporates cross-jurisdiction summary:",
        f"  {len(oc_links):>5}  total OC identity link(s)",
        f"  {len(confirmed):>5}  confirmed / auto-linked  (score ≥ 0.85)",
        f"  {len(pending):>5}  pending analyst review",
        "",
    ]

    if quota_rows:
        ev.append("  Monthly API quota usage (oc_quota table):")
        for qr in quota_rows:
            ev.append(f"    {qr.get('year_month', '?')}  →  {qr.get('request_count', 0)} requests")
    else:
        ev.append("  oc_quota table: empty (adapter has not run or is disabled)")
        ev.append("  Requires: enabled=true + oc_open_data_confirmed=true + api_key in config")

    if not oc_links and not quota_rows:
        return Finding(
            "phase20-opencorporates",
            "OpenCorporates: cross-jurisdiction company lookup for high-risk procurement entities.",
            "DATA_GAP", "LOW",
            "No OpenCorporates identity links found and oc_quota table is empty. "
            "Adapter is disabled by default — requires oc_open_data_confirmed=true and api_key.",
            evidence=ev, confidence="high", tier="osint",
        )

    if confirmed:
        ev.append("")
        ev.append("  Cross-jurisdiction matches confirmed:")
        for r in confirmed[:5]:
            src = _short_node_id(r.get("source_node_id", ""))
            dst = _short_node_id(r.get("target_node_id", ""))
            conf = r.get("confidence", 0.0)
            evd = _parse_evidence(r.get("evidence_json", ""))
            jurisdiction = evd.get("jurisdiction_code", "")
            line = f"    {conf:.3f}  {src}  →  {dst}"
            if jurisdiction:
                line += f"  [{jurisdiction}]"
            ev.append(line)

    verdict = "HIT" if confirmed else ("WEAK" if pending else "CLEAR")
    severity = "HIGH" if confirmed else "LOW"
    summary = (
        f"{len(confirmed)} confirmed cross-jurisdiction match(es). "
        f"{len(pending)} pending. "
        + (f"Quota usage: {quota_rows[0].get('request_count', 0)} requests this month." if quota_rows else "Adapter not run.")
    )
    return Finding(
        "phase20-opencorporates",
        "OpenCorporates: cross-jurisdiction company lookup for high-risk procurement entities.",
        verdict, severity, summary, evidence=ev, confidence="high", tier="osint",
    )


def probe_donation_contract_correlation(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 20 POL-04 — Political Donation ↔ Contract Correlation detector.

    Queries MotifDetected events for donation_contract_correlation alerts — fired
    when a political donor receives a contract from a party-controlled LPA within
    730 days of a donation (both directions: donation→contract and contract→donation).
    """
    # CR-02: row-returning, so `kind` is filtered in Python rather than paired with
    # the CONTAINS scan in the WHERE clause.
    rows = [
        r
        for r in db.query(
            "SELECT payload.result_json AS rj, kind, timestamp "
            "FROM graph_events "
            "WHERE payload.result_json CONTAINS '\"donation_contract_correlation\"' "
            "LIMIT 200;"
        )
        if r.get("kind") == "MotifDetected"
    ]

    if not rows:
        # Check if EC adapter has run at all (data-gap vs clean)
        ec_links = fetch_identity_links(db, source_filter="electoral_commission", limit=1)
        if not ec_links:
            return Finding(
                "phase20-donation-correlation",
                "POL-04: Political donation ↔ contract correlation within 730-day window.",
                "DATA_GAP", "HIGH",
                "No donation_contract_correlation alerts and no EC donation links. "
                "Run `urban-integrity enrich --source electoral` first, then re-run the engine.",
                evidence=[
                    "Detector: donation_correlation.rs (Phase 20 POL-04)",
                    "Window: 730 days bidirectional (donation→contract OR contract→donation)",
                    "Gate: ENR-11 (entity must be Active, not dissolved)",
                    "Party map: data/party_authority_control.json (6 parties, Open Council Data UK 2025)",
                ],
                confidence="high", tier="osint",
            )
        return Finding(
            "phase20-donation-correlation",
            "POL-04: Political donation ↔ contract correlation within 730-day window.",
            "CLEAR", "INFO",
            "EC donation links present but no correlation alerts fired. "
            "Either no donations fall within 730 days of a contract, or entity ENR-11 gate filtered them.",
            confidence="high", tier="osint",
        )

    parsed: list[dict] = []
    for r in rows:
        rj = r.get("rj", "")
        d = _parse_evidence(rj) if isinstance(rj, str) else (rj if isinstance(rj, dict) else {})
        if d:
            d["_ts"] = r.get("timestamp")
            parsed.append(d)

    # Deduplicate by motif_hash
    seen_hashes: set = set()
    unique: list[dict] = []
    for p in parsed:
        h = p.get("motif_hash", "")
        if h not in seen_hashes:
            seen_hashes.add(h)
            unique.append(p)

    # Group by party for overview
    by_party: Counter = Counter(p.get("party", "unknown") for p in unique)
    by_direction: Counter = Counter(p.get("direction", "unknown") for p in unique)

    ev = [
        f"⚠  {len(unique)} UNIQUE donation↔contract correlation alert(s)  "
        f"({len(parsed)} total incl. duplicates)",
        "",
        "  730-day window: donation precedes OR follows contract from party-controlled LPA.",
        "  Both directions trigger HIGH alert (revolving-door detection).",
        "",
        "  By party:",
    ]
    for party, n in by_party.most_common(6):
        ev.append(f"    {n:>4}  {party}")
    ev.append("")
    ev.append("  By direction:")
    for direction, n in by_direction.most_common():
        ev.append(f"    {n:>4}  {direction}")
    ev.append("")

    for p in unique[:8]:
        donor = _short_node_id(p.get("donor_id", p.get("entity_id", "")))
        party = p.get("party", "?")
        lpa = p.get("lpa_code", p.get("lpa", "?"))
        days_gap = p.get("days_between", p.get("days_gap", "?"))
        direction = p.get("direction", "?")
        ev.append(f"  DONOR    : {donor}")
        ev.append(f"  PARTY    : {party}  →  LPA: {lpa}")
        ev.append(f"  GAP      : {days_gap} days  ({direction})")
        ev.append(f"  ACTION   : `urban-integrity verify --company-number <CH_NUM>` to confirm")
        ev.append("")
    if len(unique) > 8:
        ev.append(f"  ... and {len(unique) - 8} more")

    return Finding(
        "phase20-donation-correlation",
        "POL-04: Political donation ↔ contract correlation within 730-day window.",
        "HIT", "HIGH",
        f"{len(unique)} political donation↔contract correlation alert(s) — "
        f"donors receiving contracts from party-controlled LPAs within 730 days. "
        f"Parties implicated: {', '.join(p for p, _ in by_party.most_common(3))}.",
        evidence=ev, confidence="high", tier="osint",
    )


def probe_evasion_alerts(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 17 — Evasion detector: MotifDetected alerts for active ban evasion.

    Queries graph_events for MotifDetected events written by the Disqualified Director
    Evasion detector (result_json contains motif_type='disqualified_director_evasion').
    These are the most severe findings — a director actively evading a live ban.
    """
    # CR-02: row-returning, so `kind` is filtered in Python rather than paired with
    # the CONTAINS scan in the WHERE clause.
    rows = [
        r
        for r in db.query(
            "SELECT payload.result_json AS rj, kind, timestamp "
            "FROM graph_events "
            "WHERE payload.result_json CONTAINS '\"disqualified_director_evasion\"' "
            "LIMIT 200;"
        )
        if r.get("kind") == "MotifDetected"
    ]

    if not rows:
        # The previous text offered two causes and measured neither: "either no disqualified
        # director links exist (run `urban-integrity enrich --source all`), or no confirmed
        # disqualified director is currently Active within their ban window." Only the second
        # means the detector actually ran, and the suggested remedy is a no-op for every other
        # cause — re-running `enrich` cannot help when the adapter's input set is empty.
        chain = _disqualification_chain(db)
        # `no-live-ban` is the only branch where the detector reached its own logic and returned
        # nothing. That is a clean result. Everything else is an unreachable detector.
        if chain["cause"] == "no-live-ban":
            verdict, severity = "CLEAR", "INFO"
            headline = (
                "No disqualified_director_evasion alerts — the detector ran and nobody qualifies."
            )
        elif chain["cause"] == "no-register-overlap":
            verdict, severity = "CLEAR", "INFO"
            headline = (
                "No disqualified_director_evasion alerts — nothing in the register links to "
                "this graph, so the detector had no input."
            )
        else:
            verdict, severity = "DATA_GAP", "HIGH"
            headline = (
                "No disqualified_director_evasion alerts — the detector is unreachable, not clean."
            )
        ev = [
            f"Officer nodes:                {chain['officer_nodes']:,}",
            f"director:* Entity nodes:      {chain['director_entities']:,}",
            f"ch_disqualified links:        {len(chain['links']):,} "
            f"({len(chain['elevated']):,} auto_linked/confirmed)",
            f"DisqualifiedFrom edges:       {chain['disq_edges']:,}",
            f"ch_disqualified run marker:   {chain['day'] or 'absent — adapter never ran'}",
        ]
        return Finding(
            "osint-evasion-alerts",
            "Evasion detector: active directors subject to live disqualification orders.",
            verdict, severity,
            f"{headline} {chain['explain']}",
            evidence=ev, confidence="high", tier="osint",
        )

    # Parse each result_json blob
    parsed: list[dict] = []
    for r in rows:
        rj = r.get("rj", "")
        d = _parse_evidence(rj) if isinstance(rj, str) else (rj if isinstance(rj, dict) else {})
        if d:
            d["_ts"] = r.get("timestamp")
            parsed.append(d)

    # Deduplicate by motif_hash
    seen_hashes: set = set()
    unique: list[dict] = []
    for p in parsed:
        h = p.get("motif_hash", "")
        if h not in seen_hashes:
            seen_hashes.add(h)
            unique.append(p)

    ev = [
        f"⚠  {len(unique)} UNIQUE evasion alert(s)  ({len(parsed)} total incl. duplicates)",
        "",
        "  Each entry represents an active director subject to a live Insolvency Service",
        "  disqualification order (CDDA 1986) who is currently directing a procurement entity.",
        "",
    ]
    for p in unique[:10]:
        officer = _short_node_id(p.get("officer_id", ""))
        disq_node = _short_node_id(p.get("disq_node_id", ""))
        desc = p.get("description", "")
        ev.append(f"  OFFICER : {officer}")
        ev.append(f"  DISQ    : {disq_node}")
        if desc:
            ev.append(f"  DETAIL  : {desc}")
        ev.append(f"  ACTION  : `urban-integrity verify --company-number <CH_NUM>` to confirm")
        ev.append("")
    if len(unique) > 10:
        ev.append(f"  ... and {len(unique) - 10} more")

    return Finding(
        "osint-evasion-alerts",
        "Evasion detector: active directors subject to live disqualification orders.",
        "HIT", "CRITICAL",
        f"{len(unique)} confirmed evasion alert(s) — directors actively evading live ban orders. "
        "Immediate escalation warranted under CDDA 1986.",
        evidence=ev, confidence="high", tier="osint",
    )


def probe_land_flip_candidates(db: SurrealClient, st: Strategy) -> Finding:
    """Phase 23 FLIP-02/04: pre-permission land-flip review queue.

    Reports staged Land Registry Price Paid Data (`ppd_transactions`) and the
    three-gate detector's output (`land_flip_candidates`). Candidates are a
    review-queue signal (severity ceiling MEDIUM, `buyer_identity_confirmed: false`)
    surfaced via `urban-integrity flips list` — never the alerts path.
    """
    hyp = "Land bought cheap then flipped at a markup just before a planning permission."

    # PPD staging count (the gate-1 corpus). Missing table → not staged yet.
    try:
        rows = db.query("SELECT count() AS n FROM ppd_transactions GROUP ALL;")
        ppd_n = rows[0]["n"] if rows else 0
    except RuntimeError:
        ppd_n = 0

    try:
        rows = db.query("SELECT count() AS n FROM land_flip_candidates GROUP ALL;")
        cand_n = rows[0]["n"] if rows else 0
    except RuntimeError:
        cand_n = 0

    if ppd_n == 0:
        return Finding(
            "land-flip-candidates", hyp,
            "DATA_GAP", "INFO",
            "No staged PPD transactions — the land-flip detector cannot fire. "
            "Run `urban-integrity enrich --source ppd` (set `[enricher.ppd] enabled = true`).",
            confidence="high", tier="osint",
        )

    if cand_n == 0:
        return Finding(
            "land-flip-candidates", hyp,
            "CLEAR", "INFO",
            f"{ppd_n:,} PPD transactions staged; the three-gate detector emitted zero "
            f"candidates. Expected on a sparse graph — all of address + temporal + "
            f"corroboration must pass (conservative by design). "
            f"Run `urban-integrity enrich --source land-flip` to refresh.",
            confidence="high", tier="osint",
        )

    # Candidates exist — break down by status / tier / severity and sample pending ones.
    def _group(field_name: str) -> list:
        try:
            return db.query(
                f"SELECT {field_name} AS k, count() AS n FROM land_flip_candidates "
                f"GROUP BY {field_name} ORDER BY n DESC;"
            ) or []
        except RuntimeError:
            return []

    by_status = _group("status")
    by_tier = _group("tier")
    by_severity = _group("severity")
    pending_n = next((r.get("n", 0) for r in by_status if r.get("k") == "pending_review"), 0)

    try:
        pending = db.query(
            "SELECT * FROM land_flip_candidates WHERE status = 'pending_review' "
            "ORDER BY created_at DESC LIMIT 10;"
        ) or []
    except RuntimeError:
        pending = []

    ev = [
        f"  {cand_n} land-flip candidate(s); {pending_n} pending review "
        f"(manage with `urban-integrity flips list` / `flips show <id>`)",
        f"  {ppd_n:,} PPD transactions staged (gate-1 corpus)",
        "",
        "  By tier:" ,
    ] + [f"    {r.get('n', 0):>4}  {r.get('k', '?')}" for r in by_tier]
    ev += ["", "  By severity (ceiling is MEDIUM — never HIGH):"]
    ev += [f"    {r.get('n', 0):>4}  {r.get('k', '?')}" for r in by_severity]
    if pending:
        ev += ["", "  Top pending (newest first):"]
        for r in pending:
            cid = str(r.get("id", ""))
            tier = r.get("tier", "?")
            sev = r.get("severity", "?")
            ref = r.get("planning_ref", "?")
            corr = r.get("corroborating_link_confidence")
            corr_s = f"{corr:.3f}" if isinstance(corr, (int, float)) else "n/a"
            ev.append(f"    {tier:7} {sev:6} ref={ref}  corr={corr_s}")
            ev.append(f"      id: {cid}")

    # FLIP-04: candidates are review-queue artifacts, MEDIUM-ceilinged, never auto-named.
    return Finding(
        "land-flip-candidates", hyp,
        "HIT", "MEDIUM",
        f"{cand_n} pre-permission land-flip candidate(s) ({pending_n} pending). "
        f"These are review-queue signals (MEDIUM ceiling, buyer identity unconfirmed) — "
        f"adjudicate with `urban-integrity flips confirm|dismiss <id> --note ...`.",
        evidence=ev, confidence="high", tier="osint",
    )


# ──────────────────────────────────────────────────────────────────────────
# Probe registry
# ──────────────────────────────────────────────────────────────────────────

GRAPH_PROBES = [
    probe_precomputed_alerts,
    probe_identity_integrity,
    probe_detector_readiness,
    probe_shared_exact_address,
    probe_geographic_concentration,
    probe_ward_concentration,
    probe_temporal_burst,
]

OSINT_PROBES = [
    probe_identity_link_registry,                   # Must run first — sets st.osint_active
    probe_evasion_alerts,                           # Phase 17: CRITICAL evasion MotifDetected alerts
    probe_disqualified_active_in_procurement,       # Phase 17: disq directors × contract awards
    probe_psc_beneficial_ownership,                 # Phase 17: beneficial ownership via ch_psc
    probe_gazette_insolvency_signals,               # Phase 18: Gazette InLiquidation × contracts
    probe_property_nodes,                           # Phase 18: CCOD property nodes (DATA_GAP until key set)
    probe_electoral_commission_donations,           # Phase 20 POL-01: EC donations × procurement
    probe_council_registers_political_actors,       # Phase 20 POL-02: councillor declarations
    probe_opencorporates_cross_jurisdiction,        # Phase 20 POL-03: cross-jurisdiction lookup
    probe_donation_contract_correlation,            # Phase 20 POL-04: 730-day correlation alerts
    probe_osint_confirmed_intel,                    # All confirmed/auto-linked resolutions
    probe_analyst_review_queue,                     # Pending review backlog
    probe_osint_new_node_types,                     # PSC, Charity, Property, Offshore node coverage
    probe_disqualified_director_edges,              # DisqualifiedFrom + Controls + OffshoreLink edges
    probe_land_flip_candidates,                     # Phase 23 FLIP-02/04: pre-permission land flips
]


# ──────────────────────────────────────────────────────────────────────────
# Orchestration
# ──────────────────────────────────────────────────────────────────────────

def run() -> tuple[list[Finding], list[Finding], Strategy, SurrealClient]:
    db = SurrealClient()
    st = Strategy()
    graph_findings: list[Finding] = []
    osint_findings: list[Finding] = []

    print("\n── Tier 1: Graph-event probes ──", file=sys.stderr)
    for probe in GRAPH_PROBES:
        try:
            f = probe(db, st)
        except Exception as e:
            f = Finding(probe.__name__, "(probe raised)", "DATA_GAP", "INFO",
                        f"Probe error: {e}")
        graph_findings.append(f)
        print(f"  [{f.verdict:8}] {f.severity:8} {f.probe}: {f.summary[:80]}", file=sys.stderr)

    print("\n── Tier 2: OSINT enrichment probes ──", file=sys.stderr)
    for probe in OSINT_PROBES:
        try:
            f = probe(db, st)
        except Exception as e:
            f = Finding(probe.__name__, "(probe raised)", "DATA_GAP", "INFO",
                        f"Probe error: {e}", tier="osint")
        osint_findings.append(f)
        print(f"  [{f.verdict:8}] {f.severity:8} {f.probe}: {f.summary[:80]}", file=sys.stderr)

    def sort_key(f: Finding):
        return (SEV_ORDER.get(f.severity, 9),
                {"HIT": 0, "DATA_GAP": 1, "WEAK": 2, "BLOCKED": 3, "CLEAR": 4}.get(f.verdict, 9))

    graph_findings.sort(key=sort_key)
    osint_findings.sort(key=sort_key)
    return graph_findings, osint_findings, st, db


# ──────────────────────────────────────────────────────────────────────────
# Report writer
# ──────────────────────────────────────────────────────────────────────────

def write_report(graph_findings: list[Finding], osint_findings: list[Finding],
                 st: Strategy, db: SurrealClient, path: str):
    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    all_findings = graph_findings + osint_findings
    hits = [f for f in all_findings if f.verdict == "HIT"]
    gaps = [f for f in all_findings if f.verdict == "DATA_GAP"]
    weak = [f for f in all_findings if f.verdict == "WEAK"]
    osint_hits = [f for f in hits if f.tier == "osint"]
    graph_hits = [f for f in hits if f.tier == "graph"]

    L: list[str] = []

    # ── Header ──
    L += [
        "# Procurement Integrity — Evidence Report",
        "",
        f"> Generated {now} by `scripts/analyst_probe.py` against `{db.ns}/{db.db}` "
        f"({db.query_count} queries).",
        "",
        "---",
        "",
    ]

    # ── Bottom line ──
    L += ["## Bottom Line Up Front", ""]
    if hits:
        critical = [f for f in hits if f.severity == "CRITICAL"]
        if critical:
            L.append(f"🔴 **{len(critical)} CRITICAL finding(s)** — immediate escalation warranted.")
        L.append(f"**{len(hits)} concrete corruption signal(s)** found across "
                 f"{len(graph_hits)} graph probe(s) and {len(osint_hits)} OSINT probe(s). "
                 "See Confirmed Findings.")
    else:
        L.append(
            "**No hard corruption evidence established from current dataset.** "
            "The dominant finding is a data-integrity failure (applicant names not captured) "
            "that blinds identity-based detectors. "
            "Until scraper extraction and OSINT adapters are wired, attribution is not possible."
        )
    L.append("")

    # ── OSINT enrichment status dashboard ──
    L += [
        "## OSINT Enrichment Status",
        "",
        "| Dimension | Status |",
        "|-----------|--------|",
    ]
    reg_finding = next((f for f in osint_findings if f.probe == "osint-registry"), None)
    queue_finding = next((f for f in osint_findings if f.probe == "osint-analyst-queue"), None)
    node_finding = next((f for f in osint_findings if f.probe == "osint-node-coverage"), None)
    edge_finding = next((f for f in osint_findings if f.probe == "osint-risk-edges"), None)

    def _dash_verdict(f: Finding | None, label: str) -> str:
        if f is None:
            return f"| {label} | — |"
        icons = {"HIT": "🔴 HIT", "WEAK": "🟡 WEAK", "DATA_GAP": "⚪ DATA_GAP",
                 "CLEAR": "🟢 CLEAR", "BLOCKED": "⛔ BLOCKED"}
        return f"| {label} | {icons.get(f.verdict, f.verdict)} — {f.summary[:90]} |"

    evasion_dash  = next((f for f in osint_findings if f.probe == "osint-evasion-alerts"), None)
    psc_dash      = next((f for f in osint_findings if f.probe == "osint-psc-ownership"), None)
    disq_dash     = next((f for f in osint_findings if f.probe == "osint-disqualified-directors"), None)
    ec_dash       = next((f for f in osint_findings if f.probe == "phase20-ec-donations"), None)
    council_dash  = next((f for f in osint_findings if f.probe == "phase20-council-registers"), None)
    oc_dash       = next((f for f in osint_findings if f.probe == "phase20-opencorporates"), None)
    corr_dash     = next((f for f in osint_findings if f.probe == "phase20-donation-correlation"), None)

    L.append(_dash_verdict(reg_finding, "Identity link registry"))
    L.append(_dash_verdict(evasion_dash, "⚠ Evasion alerts (disqualified_director_evasion)"))
    L.append(_dash_verdict(disq_dash, "Disqualified directors in procurement"))
    L.append(_dash_verdict(psc_dash, "PSC beneficial ownership (ch_psc)"))
    if st.confirmed_link_count or (reg_finding and reg_finding.verdict == "HIT"):
        L.append(f"| Confirmed / auto-linked | 🔴 HIT — {st.confirmed_link_count} actionable resolution(s) |")
    else:
        L.append("| Confirmed / auto-linked | ⚪ DATA_GAP — no confirmed resolutions yet |")
    if st.pending_review_count > 0:
        L.append(f"| Analyst review queue | 🟡 WEAK — {st.pending_review_count} link(s) awaiting sign-off |")
    else:
        L.append("| Analyst review queue | 🟢 CLEAR — queue empty |")
    gazette_dash = next((f for f in osint_findings if f.probe == "gazette-insolvency-signals"), None)
    prop_dash    = next((f for f in osint_findings if f.probe == "property-nodes"), None)
    L.append(_dash_verdict(gazette_dash, "London Gazette (insolvency signals)"))
    L.append(_dash_verdict(prop_dash,    "Land Registry CCOD (property nodes)"))
    L.append(_dash_verdict(ec_dash,      "Phase 20: EC donations (political donor × applicant)"))
    L.append(_dash_verdict(council_dash, "Phase 20: Council Registers (councillor declarations)"))
    L.append(_dash_verdict(oc_dash,      "Phase 20: OpenCorporates (cross-jurisdiction)"))
    L.append(_dash_verdict(corr_dash,    "Phase 20: Donation↔Contract correlation (POL-04)"))
    L.append(_dash_verdict(node_finding, "OSINT node types (PSC/Charity/Property/Offshore)"))
    L.append(_dash_verdict(edge_finding, "OSINT risk edges (DisqualifiedFrom/Controls/OffshoreLink)"))
    L += [
        "",
        f"> **Enrichment engine:** `urban-integrity enrich --source all` to run all wired adapters.  ",
        f"> **Phase 20 adapters:** `--source electoral` (EC donations), `--source council_registers`,  ",
        f"> `--source opencorporates` (requires oc_open_data_confirmed=true + api_key).  ",
        f"> **Review queue:** `urban-integrity links list` to see pending links, "
        "`links confirm <id>` / `links reject <id>` to act.",
        "",
    ]

    # ── How the search adapted ──
    L += [
        "## How the Search Adapted",
        "",
        "The probe runs an ordered battery; early verdicts gate later ones.",
        "",
    ]
    pa = next((f for f in graph_findings if f.probe == "precomputed-alerts"), None)
    if pa and pa.verdict == "HIT":
        L.append(f"1. **Pre-computed alerts** — {pa.summary}")
    else:
        L.append("1. **Pre-computed alerts** — none found; analysis is fully forensic.")
    L.append("2. **Identity integrity** — whether applicants are named (hinge probe for all "
             "identity-based motifs).")
    L.append("3. **Detector readiness** — which node types exist for each detector.")
    if st.identity_blocked:
        L.append("4. **Strategy pivot** — identity attribution broken; pivoted to geographic, "
                 "ward, and temporal clustering (identity-independent).")
    L.append("5. **OSINT enrichment tier** — cross-registry identity resolution, confirmed links, "
             "analyst review queue, PSC/offshore coverage.")
    L += [""]
    if st.notes:
        L += ["Strategy log:", ""]
        L += [f"- {n}" for n in st.notes]
        L += [""]

    # ── Section helper ──
    def section(title: str, items: list[Finding], tier_label: str = ""):
        L.append(f"## {title}")
        L.append("")
        if tier_label:
            L.append(f"_{tier_label}_")
            L.append("")
        if not items:
            L.append("_None._")
            L.append("")
            return
        for f in items:
            tier_tag = " · OSINT" if f.tier == "osint" else " · Graph"
            L.append(f"### `{f.probe}` — {f.severity} ({f.verdict}, confidence {f.confidence}{tier_tag})")
            L.append("")
            L.append(f"**Hypothesis:** {f.hypothesis}")
            L.append("")
            L.append(f"**Result:** {f.summary}")
            L.append("")
            if f.evidence:
                L.append("```")
                L.extend(f.evidence)
                L.append("```")
                L.append("")

    section("Confirmed Findings (HIT)", hits)

    # Evasion alerts — standalone CRITICAL section when present
    evasion_finding = next((f for f in osint_findings if f.probe == "osint-evasion-alerts"), None)
    if evasion_finding and evasion_finding.verdict == "HIT":
        L += [
            "## ⚠ CRITICAL: Disqualified Director Evasion Alerts",
            "",
            "_These alerts require immediate escalation. Each entry represents a director_",
            "_subject to a live Insolvency Service disqualification order (CDDA 1986) who_",
            "_is currently directing a company that participates in public procurement._",
            "",
        ]
        L.append(f"**{evasion_finding.summary}**")
        L.append("")
        if evasion_finding.evidence:
            L.append("```")
            L.extend(evasion_finding.evidence)
            L.append("```")
        L.append("")
        L.append("> **Immediate action:** Run `urban-integrity verify --company-number <CH_NUM>` "
                 "for each flagged entity. Exit code 1 confirms the evasion risk. "
                 "Refer to `docs/gdpr-basis.md` for the legal basis for using this data.")
        L.append("")

    # OSINT analyst queue — standalone section when non-empty
    if queue_finding and queue_finding.verdict not in ("DATA_GAP", "CLEAR"):
        section(
            "Analyst Review Queue — Links Pending Sign-Off",
            [queue_finding],
            "Use `urban-integrity links confirm <id>` or `links reject <id>` to act on each link. "
            "Unconfirmed links are excluded from hot-graph traversal and detector scoring."
        )

    section("Suggestive Patterns (WEAK)",
            [f for f in weak if f.probe != "osint-analyst-queue"])
    section("Data-Integrity Gaps (DATA_GAP) — Audit Findings in Their Own Right", gaps)

    # ── Full probe ledger ──
    L += [
        "## Full Probe Ledger",
        "",
        "| Tier | Probe | Verdict | Severity | Summary |",
        "|------|-------|---------|----------|---------|",
    ]
    for f in all_findings:
        tier_label = "OSINT" if f.tier == "osint" else "Graph"
        s = f.summary.replace("|", "\\|")
        s = s[:120] + "..." if len(s) > 120 else s
        L.append(f"| {tier_label} | `{f.probe}` | {f.verdict} | {f.severity} | {s} |")
    L.append("")

    # ── Recommended remediation ──
    L += ["## Recommended Remediation", ""]
    n = 1
    verdict_map = {f.probe: f.verdict for f in all_findings}

    if verdict_map.get("identity-integrity") not in ("CLEAR",):
        L.append(f"{n}. **Fix applicant extraction in the planning scraper.** "
                 "Every application resolves to `director:unknown-applicant`; the Idox/Scrapling "
                 "parser is not capturing the *Applicant* field. This single defect disables "
                 "Phoenix and nepotism detection on all planning data.")
        n += 1

    if verdict_map.get("detector-readiness") == "DATA_GAP":
        L.append(f"{n}. **Start CH streaming** (`cargo run -p urban-integrity-cli -- serve`) "
                 "with `URBAN__INGEST__CH_API_KEY` set. Officer and PoliticalActor nodes must "
                 "flow into the graph before Phoenix loop and nepotism bridge detectors can fire.")
        n += 1

    if verdict_map.get("osint-registry") == "DATA_GAP":
        L.append(f"{n}. **Run Phase 17 OSINT enrichment** (CH PSC + Disqualified Directors). "
                 "Both adapters are shipped and active — they just need credentials. "
                 "Set `URBAN__ENRICHER__CH__API_KEY` (the same key used for CH streaming) and run "
                 "`urban-integrity enrich --source all`. "
                 "PSC records expose beneficial ownership chains; disqualified director records are "
                 "direct CDDA 1986 criminal-risk flags that fire the Evasion Detector.")
        n += 1

    if st.pending_review_count > 0:
        L.append(f"{n}. **Clear the analyst review queue** (`urban-integrity links list` → "
                 f"`links confirm/reject`). {st.pending_review_count} link(s) at 0.70–0.84 "
                 "confidence are excluded from hot-graph traversal until reviewed. "
                 "Start with the highest-confidence entries — they are the easiest to verify "
                 "and most likely to activate new corruption signals.")
        n += 1

    # CH enrichment coverage
    ch_enriched = 0
    try:
        r = db.query(
            "SELECT count() AS n FROM graph_events "
            "WHERE kind='NodeAdded' AND payload.weight_json CONTAINS '\"company_number\"' GROUP ALL;"
        )
        ch_enriched = r[0]["n"] if r else 0
    except Exception:
        pass

    if ch_enriched == 0:
        L.append(f"{n}. **Backfill Companies House links** (`urban-integrity backfill ch`). "
                 "No entity nodes carry a CH company number yet — dissolution status and "
                 "officer networks (the Phoenix detector's inputs) are absent. "
                 "Requires `URBAN__INGEST__CH_API_KEY`.")
        n += 1

    # Ward coverage
    wc_finding = next((f for f in graph_findings if f.probe == "ward-concentration"), None)
    ward_count, no_ward_count = 0, 0
    if wc_finding:
        m = re.match(r"(\d+) ward\(s\)", wc_finding.summary)
        if m:
            ward_count = int(m.group(1))
        m2 = re.search(r"(\d+) application\(s\) lack ward_code", wc_finding.summary)
        if m2:
            no_ward_count = int(m2.group(1))

    if ward_count < 100:
        L.append(f"{n}. **Backfill ward codes on Contracts Finder awards** "
                 "(`urban-integrity backfill ward`). Ward codes are missing on many contracts, "
                 "limiting IMD-weighted P_c scoring. No API key required.")
        n += 1

    if verdict_map.get("precomputed-alerts") != "HIT":
        L.append(f"{n}. **Re-run the detector engine** (`urban-integrity serve`) after addressing "
                 "the items above. This report should then show `precomputed-alerts = HIT` "
                 "with Phoenix loop and nepotism bridge alerts rather than a forensic-only analysis.")
        n += 1

    L.append("")

    # ── OSINT phase roadmap ──
    # Determine Phase 17 run status from osint_active flag and probe results
    phase17_psc_result  = next((f for f in osint_findings if f.probe == "osint-psc-ownership"), None)
    evasion_result      = next((f for f in osint_findings if f.probe == "osint-evasion-alerts"), None)
    gazette_result      = next((f for f in osint_findings if f.probe == "gazette-insolvency-signals"), None)
    prop_result         = next((f for f in osint_findings if f.probe == "property-nodes"), None)
    ec_result           = next((f for f in osint_findings if f.probe == "phase20-ec-donations"), None)
    council_result      = next((f for f in osint_findings if f.probe == "phase20-council-registers"), None)
    oc_result           = next((f for f in osint_findings if f.probe == "phase20-opencorporates"), None)
    corr_result         = next((f for f in osint_findings if f.probe == "phase20-donation-correlation"), None)
    landflip_result     = next((f for f in osint_findings if f.probe == "land-flip-candidates"), None)

    def _roadmap_status(finding, shipped_msg: str = "Shipped — run `enrich` to activate") -> str:
        if finding is None:
            return shipped_msg
        if finding.verdict == "DATA_GAP":
            return shipped_msg
        if finding.verdict in ("HIT", "WEAK"):
            short = finding.summary[:60]
            return f"✅ Active — {short}{'...' if len(finding.summary) > 60 else ''}"
        return "✅ Run — no findings"

    def _roadmap_status_key(finding) -> str:
        return _roadmap_status(finding, "Shipped — set `URBAN__ENRICHER__CH__API_KEY` + run `enrich`")

    L += [
        "## OSINT Enrichment Roadmap",
        "",
        "| Phase | Adapter | What it unlocks | Status |",
        "|-------|---------|-----------------|--------|",
        f"| 17 | CH PSC (Persons with Significant Control) | Beneficial ownership chains; shell-company piercing | {_roadmap_status_key(phase17_psc_result)} |",
        f"| 17 | CH Disqualified Directors + Evasion Detector | CDDA ban evasion CRITICAL alert | {_roadmap_status_key(evasion_result)} |",
        f"| 18 | Land Registry CCOD | Property Accumulation Anomaly detector | {_roadmap_status(prop_result, 'Shipped — set LAND_REGISTRY__API_KEY')} |",
        f"| 18 | London Gazette Insolvency | Tracks corporate death–rebirth Phoenix pattern | {_roadmap_status(gazette_result)} |",
        "| 19 | Charity Commission | Charity-Commercial Crossover detector | Shipped — no key required |",
        "| 19 | ICIJ Offshore Leaks | Offshore entity structures in procurement | Shipped — no key required |",
        f"| 19 | OpenSanctions / Yente PEP | Sanctions and PEP screening | Shipped — requires non_commercial_confirmed=true |",
        f"| 20 | Electoral Commission donations | Political donation ↔ contract correlation (POL-01/04) | {_roadmap_status(ec_result, 'Shipped — no key required, run `enrich --source electoral`')} |",
        f"| 20 | Council Registers (Scrapling) | Councillor declarations → nepotism bridge (POL-02) | {_roadmap_status(council_result, 'Shipped — requires Scrapling sidecar + lpa_codes config')} |",
        f"| 20 | OpenCorporates API | Cross-jurisdiction company lookup (POL-03) | {_roadmap_status(oc_result, 'Shipped — requires oc_open_data_confirmed=true + api_key')} |",
        f"| 20 | Donation↔Contract Correlation Detector | 730-day revolving-door alerts (POL-04) | {_roadmap_status(corr_result, 'Shipped — fires after EC + contract data present')} |",
        "| 22 | Land Registry OCOD (overseas owners) | Offshore property ownership in procurement (PROP-05) | Shipped — open data, no key required |",
        f"| 23 | Land Registry PPD + Pre-Permission Land Flip Detector | Land bought cheap then flipped before a planning permission (FLIP-01..05) | {_roadmap_status(landflip_result, 'Shipped — set `[enricher.ppd] enabled = true`, run `enrich --source ppd` then `--source land-flip`')} |",
        "",
        "> **All adapters phases 17–23 are shipped.** Run `urban-integrity enrich --source all`  ",
        "> to activate all keyless adapters (EC donations, Gazette, Charity, ICIJ, Council Registers).  ",
        "> Set `URBAN__ENRICHER__CH__API_KEY` for PSC + Disqualified Directors.  ",
        "> Set `URBAN__ENRICHER__LAND_REGISTRY__API_KEY` for CCOD property data.",
        "",
    ]

    with open(path, "w") as fh:
        fh.write("\n".join(L) + "\n")


def main():
    write = "--no-report" not in sys.argv
    json_out = None
    if "--json" in sys.argv:
        idx = sys.argv.index("--json")
        # Guard the bare-argv lookup (no argparse anywhere in this file — see the module
        # docstring). Unguarded, `--json` as the last argument raised IndexError, and
        # `--json --no-report` silently consumed the next flag as the output path, writing the
        # capture to a file literally named "--no-report".
        if idx + 1 >= len(sys.argv) or sys.argv[idx + 1].startswith("--"):
            print("error: --json requires a file path argument", file=sys.stderr)
            sys.exit(2)
        json_out = sys.argv[idx + 1]
    graph_findings, osint_findings, st, db = run()

    # --json is independent of --no-report, mirroring the Rust side's decoupling of
    # `--json` from `--output` (crates/cli/src/commands/probe/run.rs, plan 36-01).
    if json_out:
        with open(json_out, "w") as fh:
            json.dump([asdict(f) for f in graph_findings + osint_findings], fh, indent=2)
        print(f"\nJSON written: {json_out}", file=sys.stderr)

    if write:
        out = os.path.join(os.path.dirname(__file__), "..", "evidence-report.md")
        out = os.path.abspath(out)
        write_report(graph_findings, osint_findings, st, db, out)
        print(f"\nReport written: {out}", file=sys.stderr)

    all_findings = graph_findings + osint_findings
    hits = sum(1 for f in all_findings if f.verdict == "HIT")
    weak = sum(1 for f in all_findings if f.verdict == "WEAK")
    gaps = sum(1 for f in all_findings if f.verdict == "DATA_GAP")
    osint_hits = sum(1 for f in all_findings if f.verdict == "HIT" and f.tier == "osint")

    print(
        f"\nSummary: {hits} HIT ({osint_hits} OSINT), {weak} WEAK, {gaps} DATA_GAP "
        f"across {len(all_findings)} probes ({db.query_count} queries).",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
