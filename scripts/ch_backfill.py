#!/usr/bin/env python3
"""
ch_backfill.py — Resolve named planning applicants to Companies House entries.

Modes:
  (default)                  Search company-like planning applicants by name →
                             write GB-COH entity + officers + Directs edges.
  --officers-for-ch-entities For every existing GB-COH entity in the graph,
                             fetch active officers from CH and write Officer
                             nodes + Directs edges. No name-search needed —
                             uses the company number embedded in the entity ID.

Uses the CH REST API (api.companieshouse.gov.uk), not the streaming API.
Auth: URBAN__ENRICHER__CH__API_KEY env var (the REST key), or --api-key flag.
(URBAN__INGEST__CH_API_KEY is the streaming key, used by `serve` — not valid here.)

Usage:
    python3 scripts/ch_backfill.py [--api-key KEY] [--dry-run]
    python3 scripts/ch_backfill.py --officers-for-ch-entities [--api-key KEY] [--dry-run]
"""
from __future__ import annotations

import argparse
import functools
import json
import os
import re
import subprocess
import time
import sys
from datetime import datetime, timezone

import requests

import surreal_target

CH_API_BASE = "https://api.companieshouse.gov.uk"
# Connection target is resolved at startup from --analysis / $URBAN__ANALYSIS /
# config.toml — see scripts/surreal_target.py. It was formerly hardcoded to the
# `main` database, which predates the analysis lifecycle and is targeted by
# nothing else; there is deliberately no default now.
TARGET: "surreal_target.SurrealTarget | None" = None

COMPANY_INDICATORS = re.compile(
    r"\b(ltd|limited|plc|llp|llc|lp|inc|corp|co\.|company|group|holdings?|"
    r"properties|developments?|construction|builders?|services?|solutions?|"
    r"associates?|consultants?|contractors?|estates?|investments?|ventures?|"
    r"enterprises?|management|rccg|cic|community interest company)\b",
    re.IGNORECASE,
)

# Exact-match-only policy (2026-06-13): this offline backfill helper assigns a CH
# company_number to a planning-applicant entity ONLY on an exact normalised-name match,
# mirroring the Rust `match_company` (crates/ingestor/src/backfill/ch.rs). The former
# fuzzy SequenceMatcher >= 0.82 path was name-only matching — the false-positive class
# removed from the enricher — so it is gone. Near misses are treated as no match.
# Legal-suffix tokens stripped during normalisation (matches core::normalise_name intent).
_LEGAL_SUFFIX_RE = re.compile(
    r"\b(ltd|limited|plc|llp|llc|lp|inc|corp|co|company)\b", re.IGNORECASE
)


def surreal_query(sql: str) -> list:
    r = requests.post(TARGET.url, headers=TARGET.headers(), data=sql)
    r.raise_for_status()
    return r.json()


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def slug(name: str) -> str:
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9]", "-", name.lower())).strip("-")


def normalise_name(name: str) -> str:
    """Lowercase, strip legal suffixes + punctuation, collapse whitespace.

    Mirrors the intent of core::normalise_name so exact comparison here matches the
    Rust enricher's exact-join policy.
    """
    n = _LEGAL_SUFFIX_RE.sub(" ", name.lower())
    n = re.sub(r"[^a-z0-9]+", " ", n)
    return re.sub(r"\s+", " ", n).strip()


def names_match_exact(a: str, b: str) -> bool:
    a_norm = normalise_name(a)
    return bool(a_norm) and a_norm == normalise_name(b)


def scan_events(fields: str, where: str = "", page_size: int = 1000):
    """
    Page every matching `graph_events` row, yielding them one at a time.

    Two SurrealDB 3.x defects make the obvious loop lose rows silently, and both have
    already cost this project a backfill that ran against 18% of its input (6,033 rows
    where `count()` said 33,046):

      - A page can come back SHORT of the requested LIMIT because the HTTP response
        truncates, not because the data ran out. So START advances by rows ACTUALLY
        RECEIVED, and the loop ends only on an empty page — never on a short one.
      - Without ORDER BY, page boundaries are not stable between requests, so rows can
        be skipped or repeated across pages. `id` is ordered on and must therefore
        appear in the projection (SurrealDB 3.x rejects an unselected ORDER BY idiom).

    `where` must NOT constrain the indexed `kind` alongside an unindexed CONTAINS — that
    combination makes the planner take the index path and truncate. Filter kind in Python.
    """
    start = 0
    while True:
        clause = f"WHERE {where} " if where else ""
        batch = surreal_query(
            f"SELECT id, {fields} FROM graph_events {clause}"
            f"ORDER BY id LIMIT {page_size} START {start}"
        )[0]["result"]
        if not batch:
            return
        for row in batch:
            yield row
        start += len(batch)


def fetch_entity_names(limit: int = 5000) -> list[tuple[str, str]]:
    """Return (entity_id, name) for Entity nodes that look like companies."""
    out = []
    # `kind` is filtered here rather than in the WHERE clause: pairing the indexed
    # equality with the CONTAINS below is the planner-bug shape that returned 39 rows
    # where count() reported 14,198.
    for r in scan_events(
        "kind AS kind, payload.entity_id AS eid, payload.weight_json AS w",
        where="payload.weight_json CONTAINS '\"type\":\"Entity\"'",
    ):
        if r.get("kind") != "NodeAdded":
            continue
        try:
            w = json.loads(r["w"])
            name = w.get("name", "")
            eid = r["eid"]
            if COMPANY_INDICATORS.search(name) and not eid.startswith("GB-COH:"):
                out.append((eid, name))
        except (json.JSONDecodeError, KeyError):
            continue
        if len(out) >= limit:
            break
    return out


def fetch_gb_coh_entity_numbers() -> list[str]:
    """Return CH company numbers for all GB-COH entities in the graph."""
    found: list[str] = []
    for r in scan_events("payload.entity_id AS eid", where='kind = "NodeAdded"'):
        eid = r.get("eid", "")
        if eid.startswith("GB-COH:"):
            number = eid[len("GB-COH:"):]
            if number:
                found.append(number)
    return list(dict.fromkeys(found))  # dedup, preserve order


def ch_search(name: str, api_key: str) -> dict | None:
    """Search CH for the company name; return best match or None."""
    try:
        r = requests.get(
            f"{CH_API_BASE}/search/companies",
            params={"q": name, "items_per_page": 5},
            auth=(api_key, ""),
            timeout=10,
        )
        if r.status_code == 429:
            print("  [rate-limit] sleeping 60s", flush=True)
            time.sleep(60)
            r = requests.get(
                f"{CH_API_BASE}/search/companies",
                params={"q": name, "items_per_page": 5},
                auth=(api_key, ""),
                timeout=10,
            )
        r.raise_for_status()
        items = r.json().get("items", [])
        # Exact-match-only: accept a CH result only when its normalised title is an
        # exact match for the query. One exact match -> use it; more than one (ambiguous)
        # or none -> no match (do not assign a company_number on a near miss).
        exact = [it for it in items if names_match_exact(name, it.get("title", ""))]
        if len(exact) == 1:
            return exact[0]
        if len(exact) > 1:
            print(f"  [ambiguous] {len(exact)} exact-name matches — skipping", flush=True)
    except Exception as exc:
        print(f"  [ch-search error] {exc}", flush=True)
    return None


def ch_officers(company_number: str, api_key: str) -> list[dict]:
    """Fetch active officers for a company from CH."""
    try:
        r = requests.get(
            f"{CH_API_BASE}/company/{company_number}/officers",
            params={"items_per_page": 50},
            auth=(api_key, ""),
            timeout=10,
        )
        if r.status_code == 429:
            time.sleep(60)
            r = requests.get(
                f"{CH_API_BASE}/company/{company_number}/officers",
                params={"items_per_page": 50},
                auth=(api_key, ""),
                timeout=10,
            )
        r.raise_for_status()
        return r.json().get("items", [])
    except Exception as exc:
        print(f"  [ch-officers error] {exc}", flush=True)
        return []


def write_events(events: list[dict], dry_run: bool) -> int:
    """Write NodeAdded/EdgeAdded events to SurrealDB. Returns count written."""
    if dry_run:
        for e in events:
            print(f"  [dry-run] {e['kind']} {e['payload'].get('entity_id') or e['payload'].get('src_id','')+'→'+e['payload'].get('dst_id','')}")
        return len(events)
    written = 0
    ts = int(datetime.now(timezone.utc).timestamp() * 1000)
    for ev in events:
        payload_str = json.dumps(ev["payload"]).replace("\\", "\\\\").replace('"', '\\"')
        sql = (
            f'CREATE graph_events CONTENT {{'
            f'"event_id":"{ev["event_id"]}",'
            f'"timestamp":{ts},'
            f'"kind":"{ev["kind"]}",'
            f'"payload":{json.dumps(ev["payload"])}'
            f'}}'
        )
        try:
            r = surreal_query(sql)
            if r[0]["status"] == "OK":
                written += 1
        except Exception as exc:
            print(f"  [write-error] {exc}", flush=True)
    return written


def make_entity_event(entity_id: str, name: str, company_number: str,
                       status: str, incorporation_date: str | None) -> dict:
    status_json = {"type": "Active"}
    if status == "dissolved":
        status_json = {"type": "Dissolved", "content": "1970-01-01T00:00:00Z"}
    elif status in ("liquidation", "receivership", "administration"):
        status_json = {"type": "InLiquidation", "content": "1970-01-01T00:00:00Z"}
    w: dict = {"type": "Entity", "name": name, "company_number": company_number, "status": status_json}
    if incorporation_date:
        w["incorporation_date"] = incorporation_date
    return {
        "event_id": f"ch-backfill-entity-{slug(entity_id)}",
        "kind": "NodeAdded",
        "payload": {"entity_id": entity_id, "weight_json": json.dumps(w, separators=(",", ":"))},
    }


SLUG_BIN = "target/debug/applicant-slug"


@functools.lru_cache(maxsize=None)
def applicant_slug(name: str) -> str:
    """Slug a name into the `director:{slug}` key form using the SHIPPED Rust function.

    Deliberately NOT re-implemented in Python. This codebase has two near-identical slug
    functions — `naming::slugify` joins runs with '_', `mapper::applicant_slug` with '-' —
    and planning applicants are already keyed with the latter. A Python copy that drifts
    from it (or copies the wrong one) produces a disjoint keyspace that joins nothing,
    which is precisely the defect this call fixes (planning open item 17).
    """
    if not os.path.exists(SLUG_BIN):
        raise SystemExit(
            f"{SLUG_BIN} not built — run:\n"
            f"  cargo build -p urban-integrity-ingestor --bin applicant-slug"
        )
    r = subprocess.run([SLUG_BIN], input=(name or "").replace("\n", " ") + "\n",
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"{SLUG_BIN} failed: {r.stderr[:300]}")
    out = r.stdout.splitlines()
    return out[0] if out else ""


def make_officer_event(company_number: str, officer: dict) -> tuple[dict, dict]:
    name = officer.get("name", "unknown")
    role = officer.get("officer_role", "director")
    appointed = officer.get("appointed_on")
    resigned = officer.get("resigned_on")
    # Officer nodes are keyed `director:{slug}` on the NAME ALONE — never per company.
    # The load-bearing keying rule: one human is one node, which is what makes cross-company
    # (and cross-LPA) Phoenix detection possible at all. This previously wrote
    # OFF:{company_number}-{NAME}, giving a person who directs five companies five unconnected
    # nodes, so Phoenix's seed -> Officer -> Dissolved motif could never traverse and 0 of 1,898
    # dissolved companies had a usable edge (planning open item 17).
    #
    # The slug MUST come from the shipped `mapper::applicant_slug` (hyphen-joined), not
    # `naming::slugify` (underscore-joined) and not a Python re-implementation — planning
    # applicants are already keyed with the former, and any divergence silently yields a
    # disjoint keyspace that joins nothing.
    officer_id = f"director:{applicant_slug(name)}"
    company_entity_id = f"GB-COH:{company_number}"
    w: dict = {"type": "Officer", "name": name, "officer_role": role}
    if appointed:
        w["appointed_on"] = appointed
    if resigned:
        w["resigned_on"] = resigned
    officer_node = {
        "event_id": f"ch-backfill-officer-{slug(officer_id)}",
        "kind": "NodeAdded",
        "payload": {"entity_id": officer_id, "weight_json": json.dumps(w, separators=(",", ":"))},
    }
    edge_w = {
        "type": "Directs",
        "meta": {"created_at": now_iso(), "effective_from": None, "effective_to": None},
        "data": {"details": role, "officer_role": role},
    }
    directs_edge = {
        "event_id": f"ch-backfill-directs-{slug(company_entity_id)}-{slug(officer_id)}",
        "kind": "EdgeAdded",
        "payload": {
            "src_id": company_entity_id,  # GB-COH → Officer (Phoenix traversal direction)
            "dst_id": officer_id,
            "weight_json": json.dumps(edge_w, separators=(",", ":")),
        },
    }
    return officer_node, directs_edge


def run_officers_for_ch_entities(api_key: str, dry_run: bool) -> None:
    """
    For every GB-COH entity already in the graph, fetch active officers from CH
    and write Officer nodes + Directs edges.  No name-matching needed — company
    numbers are embedded in the GB-COH entity IDs.
    """
    numbers = fetch_gb_coh_entity_numbers()
    print(f"GB-COH entities to process: {len(numbers)}", flush=True)

    total_written = 0
    for i, company_number in enumerate(numbers):
        if (i + 1) % 100 == 0:
            print(f"  [{i+1}/{len(numbers)}] ...", flush=True)
        officers = ch_officers(company_number, api_key)
        time.sleep(0.55)  # 0.55s ≈ 109 req/min, safely under 600/5min limit
        active = [o for o in officers if not o.get("resigned_on")]
        if not active:
            continue
        events: list[dict] = []
        for off in active:
            onode, oedge = make_officer_event(company_number, off)
            events.extend([onode, oedge])
        n = write_events(events, dry_run)
        total_written += n

    print(f"\nDone: {total_written} events written for {len(numbers)} GB-COH entities.")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--api-key", default=os.environ.get("URBAN__ENRICHER__CH__API_KEY", ""))
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--officers-for-ch-entities",
        action="store_true",
        help="Fetch officers for all existing GB-COH entities (uses company numbers, no name-search)",
    )
    parser.add_argument(
        "--i-understand-this-refuses-names-only",
        action="store_true",
        help="Override the Open Item 22 refusal on --officers-for-ch-entities. Re-creates the 94 "
             "false officer merges split on 2026-08-11. Prefer scripts/ch_officer_coverage.py.",
    )
    surreal_target.add_analysis_arg(parser)
    args = parser.parse_args()

    global TARGET
    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner(), flush=True)

    if not args.api_key:
        print("ERROR: set URBAN__ENRICHER__CH__API_KEY (the REST key) or pass --api-key", file=sys.stderr)
        sys.exit(1)

    if args.officers_for_ch_entities:
        if not args.i_understand_this_refuses_names_only:
            print(
                "REFUSED: --officers-for-ch-entities is unsafe to run (planning open item 22).\n"
                "\n"
                "  make_officer_event keys officers `director:{applicant_slug(name)}` — on the\n"
                "  NAME ALONE. On 2026-08-11 that keying had fused 94 groups of officers who are\n"
                "  different people; scripts/split_false_merged_officers.py split them back into\n"
                "  229 distinct nodes. Re-running this path re-creates every one of those merges,\n"
                "  and a false merge is what produced the project's first (retracted) Phoenix\n"
                "  alert — T-23-FalseAccusation, the gravest failure mode this system has.\n"
                "\n"
                "  It also writes no `company_number` into the Officer weight, so every officer it\n"
                "  creates is invisible to the ch_psc adapter's company-corroborated match class.\n"
                "\n"
                "USE INSTEAD:  scripts/ch_officer_coverage.py\n"
                "\n"
                "  It keys on the two identifiers CH publishes and graph_events lacks — the\n"
                "  per-person officer id and the {month, year} date of birth — and SKIPS an\n"
                "  appointment rather than fusing it when CH publishes neither. Measured over\n"
                "  1,313 appointments: 20 merged on a matching officer_id (true cross-company\n"
                "  joins), 31 disambiguated, 26 skipped, zero known false merges. Its phases are\n"
                "  separate on purpose:\n"
                "\n"
                "    --targets   list companies with no officer edge      (no network)\n"
                "    --fetch     fetch officers into a cache              (network, no writes)\n"
                "    --analyse   quantify the fusion risk                 (no writes)\n"
                "    --write --apply   commit the events\n"
                "\n"
                "If you have read the above and still need the legacy behaviour, re-run with\n"
                "--i-understand-this-refuses-names-only.",
                file=sys.stderr,
            )
            sys.exit(2)
        print(
            "WARNING: proceeding with NAME-ONLY officer keying — this will re-fuse officers who "
            "are different people (Open Item 22).",
            file=sys.stderr,
            flush=True,
        )
        run_officers_for_ch_entities(args.api_key, args.dry_run)
        return

    entities = fetch_entity_names()
    print(f"Company-like applicants to resolve: {len(entities)}")

    total_written = 0
    matched = 0

    for i, (eid, name) in enumerate(entities):
        print(f"[{i+1}/{len(entities)}] {name!r}", flush=True)
        match = ch_search(name, args.api_key)
        time.sleep(0.55)  # 0.55s ≈ 109 req/min, safely under 600/5min limit

        if not match:
            print("  → no match", flush=True)
            continue

        company_number = match["company_number"]
        ch_title = match.get("title", name)
        ch_status = match.get("company_status", "active")
        ch_date = match.get("date_of_creation")
        print(f"  → matched {ch_title!r} ({company_number}) [exact] status={ch_status}", flush=True)
        matched += 1

        events: list[dict] = []

        # Canonical GB-COH entity
        events.append(make_entity_event(
            f"GB-COH:{company_number}", ch_title, company_number, ch_status, ch_date
        ))
        # Update the planning applicant entity with company_number
        events.append(make_entity_event(eid, name, company_number, ch_status, ch_date))

        # Officers
        officers = ch_officers(company_number, args.api_key)
        time.sleep(0.55)
        for off in officers:
            if off.get("resigned_on"):
                continue  # skip resigned officers
            onode, oedge = make_officer_event(company_number, off)
            events.extend([onode, oedge])

        print(f"  → {len(officers)} officers ({sum(1 for o in officers if not o.get('resigned_on'))} active)", flush=True)
        n = write_events(events, args.dry_run)
        total_written += n

    print(f"\nDone: {matched}/{len(entities)} matched, {total_written} events written.")


if __name__ == "__main__":
    main()
