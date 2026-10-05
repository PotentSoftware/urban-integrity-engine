#!/usr/bin/env python3
"""
normalize_officer_keys.py — Create name-keyed shared officer nodes for Phoenix detection.

The original ch_backfill.py keyed officers as OFF:{company}-{name_slug}, making each
officer company-scoped and preventing Phoenix detection across companies.

This script reads existing company-scoped officer nodes and writes:
  1. Name-only officer nodes: OFF:{name_slug}
  2. Company → officer edges: GB-COH:{company} → OFF:{name_slug}
  3. Officer → company edges: OFF:{name_slug} → GB-COH:{company}

The bidirectional edges allow Phoenix Tier B traversal:
  seed(active) → neighbors → OFF:{name} → neighbors → GB-COH:{dissolved}

Officers appearing at only one company produce self-contained pairs with no cross-company
linking — harmless but necessary for completeness.

Usage:
    python3 scripts/normalize_officer_keys.py [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import re
import uuid
from collections import defaultdict
from datetime import datetime, timezone

import requests

import surreal_target

# Connection target is resolved at startup from --analysis / $URBAN__ANALYSIS /
# config.toml — see scripts/surreal_target.py. It was formerly hardcoded to the
# `main` database, which predates the analysis lifecycle and is targeted by
# nothing else; there is deliberately no default now.
TARGET: "surreal_target.SurrealTarget | None" = None

OFF_PATTERN = re.compile(r"^OFF:([^-]+)-(.+)$")


def surreal_query(sql: str) -> list:
    r = requests.post(TARGET.url, headers=TARGET.headers(), data=sql)
    r.raise_for_status()
    return r.json()


def now_ts() -> int:
    return int(datetime.now(timezone.utc).timestamp() * 1000)


def write_event(ev: dict, dry_run: bool) -> bool:
    if dry_run:
        kind = ev["kind"]
        if kind == "NodeAdded":
            print(f"  [dry-run] NodeAdded {ev['payload']['entity_id']}")
        else:
            print(f"  [dry-run] EdgeAdded {ev['payload']['src_id']} → {ev['payload']['dst_id']}")
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


def load_officer_nodes() -> dict[str, dict]:
    """
    Load all company-scoped officer nodes from graph_events.
    Returns {entity_id: weight_dict}.
    """
    page_size = 2000
    offset = 0
    officers: dict[str, dict] = {}
    while True:
        # START advances by rows ACTUALLY RECEIVED and the loop ends only on an empty
        # page: a short page means the HTTP response truncated, not that the data ran
        # out. ORDER BY keeps page boundaries stable; `id` must be selected to order on.
        rows = surreal_query(
            f"SELECT id, payload.entity_id AS eid, payload.weight_json AS wj "
            f"FROM graph_events WHERE kind = \"NodeAdded\" "
            f"ORDER BY id LIMIT {page_size} START {offset}"
        )[0]["result"]
        if not rows:
            break
        for r in rows:
            eid = r.get("eid", "")
            if not eid.startswith("OFF:"):
                continue
            try:
                w = json.loads(r["wj"])
                officers[eid] = w
            except (json.JSONDecodeError, KeyError):
                pass
        offset += len(rows)
    return officers


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    surreal_target.add_analysis_arg(parser)
    args = parser.parse_args()

    global TARGET
    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner(), flush=True)

    print("Loading company-scoped officer nodes...", flush=True)
    officers = load_officer_nodes()
    print(f"  Found {len(officers)} company-scoped OFF: nodes", flush=True)

    # Group by name slug → list of (company_number, weight_dict)
    by_name: dict[str, list[tuple[str, dict]]] = defaultdict(list)
    skipped = 0
    for eid, w in officers.items():
        m = OFF_PATTERN.match(eid)
        if not m:
            skipped += 1
            continue
        company_number = m.group(1)
        name_slug = m.group(2)
        by_name[name_slug].append((company_number, w))

    shared = sum(1 for v in by_name.values() if len(v) > 1)
    print(f"  {len(by_name)} unique name slugs ({shared} shared across multiple companies)", flush=True)
    if skipped:
        print(f"  {skipped} entries skipped (unexpected format)", flush=True)

    # Write events
    written_nodes = 0
    written_edges = 0

    for name_slug, entries in by_name.items():
        canonical_id = f"OFF:{name_slug}"

        # Use the weight from the first occurrence (they should all be the same person)
        _, sample_w = entries[0]
        # Strip company-specific officer_role variation — keep name only for canonical node
        canonical_w = {
            "type": "Officer",
            "name": sample_w.get("name", name_slug),
        }
        # Preserve appointed_on from first entry if present
        if sample_w.get("appointed_on"):
            canonical_w["appointed_on"] = sample_w["appointed_on"]
        if sample_w.get("officer_role"):
            canonical_w["officer_role"] = sample_w["officer_role"]

        # Write canonical officer node (once)
        node_ev = {
            "event_id": f"norm-officer-node-{name_slug[:60]}",
            "kind": "NodeAdded",
            "payload": {
                "entity_id": canonical_id,
                "weight_json": json.dumps(canonical_w, separators=(",", ":")),
            },
        }
        if write_event(node_ev, args.dry_run):
            written_nodes += 1

        # Write bidirectional edges for each company
        for company_number, _ in entries:
            gb_coh_id = f"GB-COH:{company_number}"

            # GB-COH → OFF (company→officer, for Phoenix seed→officer traversal)
            fwd_ev = {
                "event_id": f"norm-directs-fwd-{company_number}-{name_slug[:40]}",
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": gb_coh_id,
                    "dst_id": canonical_id,
                    "weight_json": json.dumps({
                        "type": "Directs",
                        "meta": {"created_at": "2026-05-25T00:00:00Z", "effective_from": None, "effective_to": None},
                        "data": {"details": "director", "officer_role": sample_w.get("officer_role", "director")},
                    }, separators=(",", ":")),
                },
            }
            if write_event(fwd_ev, args.dry_run):
                written_edges += 1

            # OFF → GB-COH (officer→company, for Phoenix officer→dissolved traversal)
            rev_ev = {
                "event_id": f"norm-directs-rev-{company_number}-{name_slug[:40]}",
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": canonical_id,
                    "dst_id": gb_coh_id,
                    "weight_json": json.dumps({
                        "type": "Directs",
                        "meta": {"created_at": "2026-05-25T00:00:00Z", "effective_from": None, "effective_to": None},
                        "data": {"details": "director", "officer_role": sample_w.get("officer_role", "director")},
                    }, separators=(",", ":")),
                },
            }
            if write_event(rev_ev, args.dry_run):
                written_edges += 1

    print(f"\nDone: {written_nodes} canonical officer nodes, {written_edges} edges written.")
    print("Restart `urban-integrity serve` to replay the new nodes and edges.")


if __name__ == "__main__":
    main()
