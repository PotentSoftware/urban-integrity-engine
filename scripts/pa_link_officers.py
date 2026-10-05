#!/usr/bin/env python3
"""
pa_link_officers.py — Link Sandwell councillors to their CH company directorships.

For each PoliticalActor node (PA:{slug}) in the graph:
  1. Search the CH officer search API for the councillor's name.
  2. Pick the officer whose name is an exact match (order-independent token-set
     equality; exact-match-only policy 2026-06-13 — no fuzzy similarity).
  3. Fetch their active appointments from CH.
  4. Write a GB-COH entity node (if absent) and a PA → GB-COH Directs edge
     for each active appointment.

This gives the NepotismBridge BFS a 2-hop path:
    PA:{slug} ─Directs→ GB-COH:{company} ─Directs→ OFF:{officer}

Uses the CH REST API (officer/appointments search), not the streaming API.
Auth: URBAN__ENRICHER__CH__API_KEY env var (the REST key), or --api-key flag.
(URBAN__INGEST__CH_API_KEY is the streaming key, used by `serve` — not valid here.)

Usage:
    python3 scripts/pa_link_officers.py [--api-key KEY] [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from datetime import datetime, timezone

import requests

import surreal_target

CH_API_BASE = "https://api.companieshouse.gov.uk"
# Connection target is resolved at startup from --analysis / $URBAN__ANALYSIS /
# config.toml — see scripts/surreal_target.py. It was formerly hardcoded to the
# `main` database, which predates the analysis lifecycle and is targeted by
# nothing else; there is deliberately no default now.
TARGET: "surreal_target.SurrealTarget | None" = None

# Exact-match-only policy (2026-06-13): councillor names are linked to CH officers ONLY
# on an exact name match. The former fuzzy SequenceMatcher >= 0.80 path was name-only
# matching — the false-positive class removed from the enricher — so it is gone. Because
# CH officer titles are formatted "SURNAME, Forename(s)" while councillor names are
# "Forename Surname", the exact comparison is order-independent token-set equality
# (every name token must match exactly; no similarity score, no phonetics, no edit distance).


def surreal_query(sql: str) -> list:
    r = requests.post(TARGET.url, headers=TARGET.headers(), data=sql)
    r.raise_for_status()
    return r.json()


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def now_ts() -> int:
    return int(datetime.now(timezone.utc).timestamp() * 1000)


def slug(name: str) -> str:
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9]", "-", name.lower())).strip("-")


def _name_tokens(name: str) -> frozenset[str]:
    """Lowercase, drop punctuation, return the set of name tokens (order-independent)."""
    cleaned = re.sub(r"[^a-z0-9]+", " ", name.lower())
    return frozenset(t for t in cleaned.split() if t)


def names_match_exact(a: str, b: str) -> bool:
    """Exact (order-independent) name equality: identical token sets, no fuzzy scoring."""
    ta = _name_tokens(a)
    return bool(ta) and ta == _name_tokens(b)


def load_pa_nodes() -> list[tuple[str, str]]:
    """Return [(entity_id, name)] for all PoliticalActor nodes."""
    # START advances by rows ACTUALLY RECEIVED and the loop ends only on an empty page:
    # a short page means the HTTP response truncated, not that the data ran out. ORDER BY
    # keeps page boundaries stable, and `id` must be selected to be ordered on.
    page_size = 2000
    offset = 0
    pa_nodes: dict[str, str] = {}  # entity_id → name (dedup: latest write wins)
    while True:
        rows = surreal_query(
            f"SELECT id, payload.entity_id AS eid, payload.weight_json AS wj "
            f"FROM graph_events WHERE kind = 'NodeAdded' "
            f"ORDER BY id LIMIT {page_size} START {offset}"
        )[0]["result"]
        if not rows:
            break
        for r in rows:
            eid = r.get("eid", "")
            if not eid.startswith("PA:"):
                continue
            try:
                w = json.loads(r["wj"])
                if w.get("type") == "PoliticalActor" and w.get("name"):
                    pa_nodes[eid] = w["name"]
            except (json.JSONDecodeError, KeyError):
                pass
        offset += len(rows)
    return list(pa_nodes.items())


def ch_search_officer(name: str, api_key: str) -> tuple[str, str] | None:
    """
    Search CH for a named officer. Returns (officer_id, ch_title) for the single
    exact name match, or None (no match, or ambiguous multiple exact matches).
    """
    try:
        r = requests.get(
            f"{CH_API_BASE}/search/officers",
            params={"q": name, "items_per_page": 5},
            auth=(api_key, ""),
            timeout=10,
        )
        if r.status_code == 429:
            print("  [rate-limit] sleeping 60s", flush=True)
            time.sleep(60)
            r = requests.get(
                f"{CH_API_BASE}/search/officers",
                params={"q": name, "items_per_page": 5},
                auth=(api_key, ""),
                timeout=10,
            )
        r.raise_for_status()
        items = r.json().get("items", [])
        # Exact-match-only: accept an officer only on an exact (order-independent) name
        # match. Exactly one exact match -> use it; more than one (ambiguous) or none ->
        # no match (never link on a near miss).
        exact = [it for it in items if names_match_exact(name, it.get("title", ""))]
        if len(exact) > 1:
            print(f"  [ambiguous] {len(exact)} exact-name officer matches — skipping", flush=True)
        elif len(exact) == 1:
            # links.self is like "/officers/{id}/appointments"
            link = exact[0].get("links", {}).get("self", "")
            officer_id = link.strip("/").split("/")[1] if link else None
            if officer_id:
                return officer_id, exact[0]["title"]
    except Exception as exc:
        print(f"  [ch-search error] {exc}", flush=True)
    return None


def ch_appointments(officer_id: str, api_key: str) -> list[dict]:
    """Fetch active appointments for an officer from CH."""
    try:
        r = requests.get(
            f"{CH_API_BASE}/officers/{officer_id}/appointments",
            params={"items_per_page": 50},
            auth=(api_key, ""),
            timeout=10,
        )
        if r.status_code == 429:
            time.sleep(60)
            r = requests.get(
                f"{CH_API_BASE}/officers/{officer_id}/appointments",
                params={"items_per_page": 50},
                auth=(api_key, ""),
                timeout=10,
            )
        r.raise_for_status()
        return [item for item in r.json().get("items", []) if not item.get("resigned_on")]
    except Exception as exc:
        print(f"  [ch-appointments error] {exc}", flush=True)
        return []


def write_event(ev: dict, dry_run: bool) -> bool:
    if dry_run:
        kind = ev["kind"]
        p = ev["payload"]
        if kind == "NodeAdded":
            print(f"  [dry-run] NodeAdded {p['entity_id']}")
        else:
            print(f"  [dry-run] EdgeAdded {p['src_id']} → {p['dst_id']}")
        return True
    ts = now_ts()
    sql = (
        f"CREATE graph_events CONTENT {{"
        f'"event_id":"{ev["event_id"]}",'
        f'"timestamp":{ts},'
        f'"kind":"{ev["kind"]}",'
        f'"payload":{json.dumps(ev["payload"])}'
        f"}}"
    )
    try:
        result = surreal_query(sql)
        return result[0]["status"] == "OK"
    except Exception as exc:
        print(f"  [write-error] {exc}", flush=True)
        return False


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-key", default=os.environ.get("URBAN__ENRICHER__CH__API_KEY", ""))
    parser.add_argument("--dry-run", action="store_true")
    surreal_target.add_analysis_arg(parser)
    args = parser.parse_args()

    global TARGET
    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner(), flush=True)

    if not args.api_key:
        print("ERROR: set URBAN__ENRICHER__CH__API_KEY (the REST key) or pass --api-key", file=sys.stderr)
        sys.exit(1)

    pa_nodes = load_pa_nodes()
    print(f"PoliticalActor nodes to process: {len(pa_nodes)}", flush=True)

    total_edges = 0
    matched = 0

    for i, (pa_id, councillor_name) in enumerate(pa_nodes):
        print(f"[{i+1}/{len(pa_nodes)}] {councillor_name!r}", flush=True)

        result = ch_search_officer(councillor_name, args.api_key)
        time.sleep(0.55)

        if not result:
            print("  → no CH officer match", flush=True)
            continue

        officer_id, ch_title = result
        print(f"  → matched {ch_title!r} (id={officer_id[:8]}…) [exact]", flush=True)

        appointments = ch_appointments(officer_id, args.api_key)
        time.sleep(0.55)

        if not appointments:
            print("  → no active appointments", flush=True)
            matched += 1
            continue

        matched += 1
        print(f"  → {len(appointments)} active appointment(s)", flush=True)

        # Canonical officer node for the councillor themselves + PA → OFF direct edge.
        # This creates the 1-hop NepotismBridge path: PA → Officer.
        off_name_slug = re.sub(r"[^A-Z0-9]", "_", ch_title.upper())
        off_id = f"OFF:{off_name_slug}"
        councillor_slug = slug(pa_id[3:])  # strip "PA:"

        off_node_ev = {
            "event_id": f"pa-link-officer-{councillor_slug}",
            "kind": "NodeAdded",
            "payload": {
                "entity_id": off_id,
                "weight_json": json.dumps({
                    "type": "Officer",
                    "name": ch_title,
                    "officer_role": "director",
                }, separators=(",", ":")),
            },
        }
        write_event(off_node_ev, args.dry_run)

        pa_off_ev = {
            "event_id": f"pa-link-directs-off-{councillor_slug}",
            "kind": "EdgeAdded",
            "payload": {
                "src_id": pa_id,
                "dst_id": off_id,
                "weight_json": json.dumps({
                    "type": "Directs",
                    "meta": {"created_at": now_iso(), "effective_from": None, "effective_to": None},
                    "data": {"details": "councillor_officer", "officer_role": "director"},
                }, separators=(",", ":")),
            },
        }
        if write_event(pa_off_ev, args.dry_run):
            total_edges += 1
            print(f"  → PA → {off_id}", flush=True)

        for appt in appointments:
            company_number = appt.get("appointed_to", {}).get("company_number", "")
            company_name = appt.get("appointed_to", {}).get("company_name", "")
            officer_role = appt.get("officer_role", "director")
            if not company_number:
                continue

            gb_coh_id = f"GB-COH:{company_number}"
            company_slug = slug(pa_id[3:])  # strip "PA:" prefix

            # GB-COH entity node (idempotent — will be ignored if already present)
            node_ev = {
                "event_id": f"pa-link-entity-{company_number}",
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": gb_coh_id,
                    "weight_json": json.dumps({
                        "type": "Entity",
                        "name": company_name,
                        "company_number": company_number,
                        "status": {"type": "Active"},
                    }, separators=(",", ":")),
                },
            }
            write_event(node_ev, args.dry_run)

            # PA → GB-COH Directs edge
            edge_ev = {
                "event_id": f"pa-link-directs-{company_slug}-{company_number}",
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": pa_id,
                    "dst_id": gb_coh_id,
                    "weight_json": json.dumps({
                        "type": "Directs",
                        "meta": {
                            "created_at": now_iso(),
                            "effective_from": None,
                            "effective_to": None,
                        },
                        "data": {"details": "councillor_directorship", "officer_role": officer_role},
                    }, separators=(",", ":")),
                },
            }
            if write_event(edge_ev, args.dry_run):
                total_edges += 1
                print(f"    → PA → {gb_coh_id} ({company_name[:40]})", flush=True)

    print(f"\nDone: {matched}/{len(pa_nodes)} councillors matched, {total_edges} PA→GB-COH edges written.")
    print("Restart `urban-integrity serve` to replay the new edges.")


if __name__ == "__main__":
    main()
