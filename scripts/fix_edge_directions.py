#!/usr/bin/env python3
"""
fix_edge_directions.py — Add missing forward edges for Phoenix detection.

Two fixes:
  1. Directs edges: ch_backfill wrote OFF→GB-COH but Phoenix needs GB-COH→OFF
     (outgoing from active company). Writes reverse edges with '-fwd' suffix.
  2. AwardedTo bridge: CF parties (CF-PARTY:GB-CFS-NNN) with a known CH
     company_number get bridging GB-COH→Contract edges so Phoenix can find
     contracts via the canonical GB-COH entity ID.

Usage:
    python3 scripts/fix_edge_directions.py [--dry-run]
"""
from __future__ import annotations

import argparse
import json
import uuid
from datetime import datetime, timezone

import requests

import surreal_target

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


def write_event(ev: dict, dry_run: bool) -> bool:
    if dry_run:
        print(f"  [dry-run] {ev['kind']} {ev['payload'].get('src_id','')}→{ev['payload'].get('dst_id','')}")
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


# ── Fix 1: Directs forward edges ──────────────────────────────────────────────

def add_forward_directs_edges(dry_run: bool) -> int:
    """
    For every Directs EdgeAdded event (OFF:→GB-COH:), write a new event
    with src/dst swapped so Phoenix can traverse GB-COH:→OFF:.
    """
    print("Fix 1: adding forward Directs edges (GB-COH → OFF) ...", flush=True)
    page_size = 1000
    offset = 0
    written = 0
    seen_pairs: set[tuple[str, str]] = set()

    while True:
        batch = surreal_query(
            f'SELECT id, payload FROM graph_events WHERE kind = "EdgeAdded" '
            f'ORDER BY id LIMIT {page_size} START {offset}'
        )[0]["result"]
        if not batch:
            break

        for row in batch:
            payload = row.get("payload", {})
            if "Directs" not in payload.get("weight_json", ""):
                continue
            src = payload.get("src_id", "")
            dst = payload.get("dst_id", "")
            if not (src.startswith("OFF:") and dst.startswith("GB-COH:")):
                continue
            pair = (dst, src)  # forward direction: GB-COH → OFF
            if pair in seen_pairs:
                continue
            seen_pairs.add(pair)

            ev = {
                "event_id": f"fwd-directs-{str(uuid.uuid4())}",
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": dst,   # GB-COH entity
                    "dst_id": src,   # Officer
                    "weight_json": payload["weight_json"],
                },
            }
            if write_event(ev, dry_run):
                written += 1

        # A short page means the response truncated, not that the data ran out —
        # advance by rows ACTUALLY RECEIVED and stop only on an empty page.
        offset += len(batch)

    print(f"  → {written} forward Directs edges written", flush=True)
    return written


# ── Fix 2: GB-COH → Contract AwardedTo bridge ────────────────────────────────

def add_gbcoh_contract_bridges(dry_run: bool) -> int:
    """
    For CF-PARTY entities that have a company_number, find their AwardedTo
    edges and write matching edges from GB-COH:{company_number} → contract.
    """
    print("Fix 2: adding GB-COH → Contract bridge edges ...", flush=True)

    # Build map: CF-PARTY entity_id → company_number
    party_to_ch: dict[str, str] = {}
    page_size = 1000
    offset = 0
    while True:
        batch = surreal_query(
            f'SELECT id, payload FROM graph_events WHERE kind = "NodeAdded" '
            f'ORDER BY id LIMIT {page_size} START {offset}'
        )[0]["result"]
        if not batch:
            break
        for row in batch:
            p = row.get("payload", {})
            eid = p.get("entity_id", "")
            if not eid.startswith("CF-PARTY:"):
                continue
            try:
                w = json.loads(p.get("weight_json", "{}"))
                cn = w.get("company_number")
                if cn:
                    party_to_ch[eid] = cn
            except json.JSONDecodeError:
                pass
        # A short page means the response truncated, not that the data ran out —
        # advance by rows ACTUALLY RECEIVED and stop only on an empty page.
        offset += len(batch)

    print(f"  CF-PARTY entities with company_number: {len(party_to_ch)}", flush=True)
    if not party_to_ch:
        return 0

    # Now find AwardedTo edges from known CF-PARTY entities
    written = 0
    seen_pairs: set[tuple[str, str]] = set()
    offset = 0
    while True:
        batch = surreal_query(
            f'SELECT id, payload FROM graph_events WHERE kind = "EdgeAdded" '
            f'ORDER BY id LIMIT {page_size} START {offset}'
        )[0]["result"]
        if not batch:
            break

        for row in batch:
            payload = row.get("payload", {})
            if "AwardedTo" not in payload.get("weight_json", ""):
                continue
            src = payload.get("src_id", "")  # CF-PARTY entity
            dst = payload.get("dst_id", "")  # CF contract
            if src not in party_to_ch:
                continue
            gb_coh_id = f"GB-COH:{party_to_ch[src]}"
            pair = (gb_coh_id, dst)
            if pair in seen_pairs:
                continue
            seen_pairs.add(pair)

            ev = {
                "event_id": f"gbcoh-contract-{str(uuid.uuid4())}",
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": gb_coh_id,
                    "dst_id": dst,
                    "weight_json": payload["weight_json"],
                },
            }
            if write_event(ev, dry_run):
                written += 1

        # A short page means the response truncated, not that the data ran out —
        # advance by rows ACTUALLY RECEIVED and stop only on an empty page.
        offset += len(batch)

    print(f"  → {written} GB-COH → Contract bridge edges written", flush=True)
    return written


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    surreal_target.add_analysis_arg(parser)
    args = parser.parse_args()

    global TARGET
    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner(), flush=True)

    n1 = add_forward_directs_edges(args.dry_run)
    n2 = add_gbcoh_contract_bridges(args.dry_run)
    print(f"\nTotal: {n1 + n2} events written.")

    if not args.dry_run and (n1 + n2) > 0:
        print("Restart `urban-integrity serve` so the coordinator replays the new edges.")


if __name__ == "__main__":
    main()
