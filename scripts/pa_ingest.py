#!/usr/bin/env python3
"""
pa_ingest.py — Ingest elected Sandwell councillors as PoliticalActor nodes.

Scrapes the Sandwell ModernGov member index (single public page, no auth) to
obtain the current 72 sitting councillors, then writes NodeAdded events to
SurrealDB graph_events.

This unblocks the nepotism-bridge detector, which BFS starts from PoliticalActor
nodes.  A nepotism alert fires when a planning applicant and a sitting councillor
share a director relationship.

EntityId format : PA:{slug(person_name)}
NodeWeight JSON : {"type":"PoliticalActor","name":"<name>"}

Usage:
    python3 scripts/pa_ingest.py [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone

import requests

import surreal_target

MODERNGOV_URL = "https://sandwell.moderngov.co.uk/mgMemberIndex.aspx"
MODERNGOV_HEADERS = {
    "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
}

# Connection target is resolved at startup from --analysis / $URBAN__ANALYSIS /
# config.toml — see scripts/surreal_target.py. It was formerly hardcoded to the
# `main` database, which predates the analysis lifecycle and is targeted by
# nothing else; there is deliberately no default now.
TARGET: "surreal_target.SurrealTarget | None" = None


def surreal_query(sql: str) -> list:
    r = requests.post(TARGET.url, headers=TARGET.headers(), data=sql)
    r.raise_for_status()
    return r.json()


def now_ts() -> int:
    return int(datetime.now(timezone.utc).timestamp() * 1000)


def slug(name: str) -> str:
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9]", "-", name.lower())).strip("-")


def fetch_councillors() -> list[dict]:
    """
    Scrape the Sandwell ModernGov member index and return a list of
    {"uid": str, "name": str} for each sitting councillor.

    The page lists all 72 elected councillors with their ModernGov UID and
    display name (e.g. "Councillor Jane Example").  Councillor prefix stripped
    from the stored name.
    """
    r = requests.get(MODERNGOV_URL, headers=MODERNGOV_HEADERS, timeout=20)
    r.raise_for_status()

    # Each councillor: <a href="mgUserInfo.aspx?UID=NNN" ...><img ... /><br />Councillor Name</a>
    matches = re.findall(
        r'href="mgUserInfo\.aspx\?UID=(\d+)"[^>]*>.*?<br />(.*?)</a>',
        r.text, re.DOTALL
    )
    councillors = []
    for uid, raw_name in matches:
        name = raw_name.strip()
        # Strip "Councillor(s) " or "Cllr " honorific — store just the person's name
        name = re.sub(r"^(Councillors?|Cllr)\s+", "", name, flags=re.IGNORECASE)
        if name:
            councillors.append({"uid": uid, "name": name})

    return councillors


def make_node_event(uid: str, name: str) -> dict:
    entity_id = f"PA:{slug(name)}"
    weight = {"type": "PoliticalActor", "name": name}
    return {
        "event_id": f"pa-ingest-mg-{uid}",
        "kind": "NodeAdded",
        "payload": {
            "entity_id": entity_id,
            "weight_json": json.dumps(weight, separators=(",", ":")),
        },
    }


def write_events(events: list[dict], dry_run: bool) -> int:
    if dry_run:
        for ev in events:
            print(f"  [dry-run] {ev['kind']} {ev['payload']['entity_id']}")
        return len(events)

    ts = now_ts()
    written = 0
    for ev in events:
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
            if result[0]["status"] == "OK":
                written += 1
            else:
                print(f"  [surreal-error] {result[0]}", flush=True)
        except Exception as exc:
            print(f"  [write-error] {exc}", flush=True)
    return written


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true",
                        help="Print events without writing to SurrealDB")
    surreal_target.add_analysis_arg(parser)
    args = parser.parse_args()

    global TARGET
    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner(), flush=True)

    print(f"Fetching councillors from {MODERNGOV_URL} ...", flush=True)
    councillors = fetch_councillors()

    if not councillors:
        print("ERROR: no councillors parsed — page structure may have changed", file=sys.stderr)
        sys.exit(1)

    print(f"  {len(councillors)} councillors found")

    events = [make_node_event(c["uid"], c["name"]) for c in councillors]

    print("\nSample events:")
    for ev in events[:4]:
        print(f"  {ev['payload']['entity_id']}  →  {ev['payload']['weight_json']}")

    print(f"\nWriting {len(events)} PoliticalActor nodes...", flush=True)
    written = write_events(events, args.dry_run)
    print(f"Done: {written}/{len(events)} events written.")

    if not args.dry_run and written > 0:
        print(
            "\nRestart `urban-integrity serve` (or wait for next snapshot interval) so the"
            "\ncoordinator replays these NodeAdded events and the nepotism-bridge detector"
            "\ncan traverse PoliticalActor → Officer → Entity paths."
        )


if __name__ == "__main__":
    main()
