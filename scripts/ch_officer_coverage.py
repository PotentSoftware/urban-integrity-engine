#!/usr/bin/env python3
"""
ch_officer_coverage.py — close Phoenix's real gap: CF awardee companies with no officers.

`scripts/phoenix_funnel.py` measured where Phoenix candidates die. 46,861 of 46,953
active seeds never reach a dissolved company via a shared officer, and 74% of
Contracts Finder awardees (3,234 of 4,371) carry no `Directs` edge at all. That
missing officer coverage — not contract dates — is what stops Phoenix.

This tool fills it, in three separate phases so the risky part is measured before
it happens:

  --targets   list the CF awardee companies that have no officer edge   (no network)
  --fetch     fetch their officers from the CH REST API into a cache    (network, no writes)
  --analyse   report how many new officer keys would FUSE with an       (no network)
              existing node, and how many of those fusions join people
              CH says are different humans
  --write     write the Officer + Directs events                        (mutates the graph)

Why the phases are separate: planning open item 22 marks
`ch_backfill.py --officers-for-ch-entities` unsafe to re-run, because it keys officers
`director:{applicant_slug(name)}` — on the name alone — and would re-fuse the 94 groups
that the 2026-08-11 split repaired. That risk is real but it has never been quantified
for a *targeted* run. `--analyse` quantifies it from the same CH payload that
`--fetch` already pays for, using the two identifiers `graph_events` lacks: CH's
per-person officer id, and the {month, year} date of birth.

Auth: URBAN__ENRICHER__CH__API_KEY (the REST key). URBAN__INGEST__CH_API_KEY is the
streaming key and 401s here.
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

import requests

import surreal_target

CH_API_BASE = "https://api.companieshouse.gov.uk"
DATA = Path(__file__).resolve().parent.parent / "data"
CACHE = DATA / "ch-officers-cf-awardees.jsonl"
#: The 2026-08-11 merge audit's cache — same shape, covers the 987 collision companies.
AUDIT_CACHE = DATA / "ch-officers-cache.jsonl"
TARGETS_FILE = DATA / "cf-awardees-without-officers.txt"

TARGET: "surreal_target.SurrealTarget | None" = None


def q(sql, timeout=1200):
    out = TARGET.query(sql, timeout=timeout)
    for r in out:
        if r.get("status") != "OK":
            raise SystemExit(f"query failed: {r}")
    return [r["result"] for r in out]


SLUG_BIN = Path(__file__).resolve().parent.parent / "target" / "debug" / "applicant-slug"
_SLUG_CACHE: dict[str, str] = {}


def slug_all(names: list[str]) -> dict[str, str]:
    """Slug names with the SHIPPED Rust `applicant_slug`, one subprocess for the batch.

    Deliberately NOT re-implemented in Python. The Rust function classifies with
    `char::is_alphanumeric`, which is Unicode-aware and so KEEPS accented letters; the
    obvious Python `re.sub(r"[^a-z0-9]+", "-", ...)` replaces them with hyphens, giving a
    different key for every non-ASCII name. That is the disjoint-keyspace defect Open
    Item 17 exists for, and `crates/ingestor/src/bin/applicant-slug.rs` exists to prevent.
    """
    todo = sorted({n for n in names if n and n not in _SLUG_CACHE})
    if todo:
        if not SLUG_BIN.exists():
            raise SystemExit(f"{SLUG_BIN} not built — run:\n"
                             f"  cargo build -p urban-integrity-ingestor --bin applicant-slug")
        r = subprocess.run([str(SLUG_BIN)], input="\n".join(n.replace("\n", " ") for n in todo) + "\n",
                           capture_output=True, text=True)
        if r.returncode != 0:
            raise SystemExit(f"{SLUG_BIN} failed: {r.stderr[:300]}")
        outs = r.stdout.splitlines()
        if len(outs) != len(todo):
            raise SystemExit(f"slug shim returned {len(outs)} lines for {len(todo)} names")
        _SLUG_CACHE.update(zip(todo, outs))
    return _SLUG_CACHE


def applicant_slug(name: str) -> str:
    if name not in _SLUG_CACHE:
        slug_all([name])
    return _SLUG_CACHE.get(name, "")


# ── phase 1: targets ────────────────────────────────────────────────────────────

def compute_targets() -> list[str]:
    """CF awardee entities that carry no Directs edge. Read from graph_events, not the
    projected tables — `directs` holds 4,367 of the log's 22,942 edges."""
    print("scanning graph_events ...", flush=True)
    nodes, = q("SELECT payload.entity_id AS eid, payload.weight_json AS w FROM graph_events "
               "WHERE kind IN ['NodeAdded','NodeUpdated'];")
    officers, contracts = set(), set()
    for r in nodes:
        eid, w = r.get("eid"), r.get("w")
        if not eid or not w:
            continue
        t = json.loads(w).get("type")
        if t == "Officer":
            officers.add(eid)
        elif t == "Contract":
            contracts.add(eid)

    edges, = q("SELECT payload.src_id AS s, payload.dst_id AS d, payload.weight_json AS w "
               "FROM graph_events WHERE kind='EdgeAdded';")
    has_officer, cf_awardees = set(), set()
    for e in edges:
        s, d, w = e.get("s"), e.get("d"), e.get("w")
        if not s or not d or not w:
            continue
        t = json.loads(w).get("type")
        if t == "Directs":
            for a, b in ((s, d), (d, s)):
                if b in officers:
                    has_officer.add(a)
        elif t == "AwardedTo":
            for a, b in ((s, d), (d, s)):
                if b in contracts and b.startswith("CF:"):
                    cf_awardees.add(a)

    targets = sorted(e for e in cf_awardees
                     if e.startswith("GB-COH:") and e not in has_officer)
    print(f"  CF awardees: {len(cf_awardees):,}   "
          f"of which GB-COH: {sum(1 for e in cf_awardees if e.startswith('GB-COH:')):,}   "
          f"already have officers: {sum(1 for e in cf_awardees if e in has_officer):,}")
    print(f"  targets (GB-COH, no officer edge): {len(targets):,}")
    return [t.split(":", 1)[1] for t in targets]


# ── phase 2: fetch ──────────────────────────────────────────────────────────────

def load_cache() -> dict[str, list]:
    cache: dict[str, list] = {}
    for path in (AUDIT_CACHE, CACHE):
        if not path.exists():
            continue
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                rec = json.loads(line)
                cache[rec["cn"]] = rec.get("items", [])
    return cache


def ch_officers(cn: str, api_key: str) -> list | None:
    """Return CH's officer items for a company, or None on a hard failure.

    None and [] are deliberately distinct: [] means CH answered and the company has no
    officers, None means we never got an answer and must not cache a false empty.
    """
    for attempt in (1, 2):
        try:
            r = requests.get(f"{CH_API_BASE}/company/{cn}/officers",
                             params={"items_per_page": 100}, auth=(api_key, ""), timeout=15)
            if r.status_code == 429:
                print("  [rate-limit] sleeping 60s", flush=True)
                time.sleep(60)
                continue
            if r.status_code == 404:
                return []
            r.raise_for_status()
            return r.json().get("items", [])
        except Exception as exc:
            if attempt == 2:
                print(f"  [error] {cn}: {exc}", flush=True)
                return None
            time.sleep(2)
    return None


def fetch(targets: list[str], api_key: str) -> None:
    cache = load_cache()
    todo = [cn for cn in targets if cn not in cache]
    print(f"cached: {len(targets) - len(todo):,}   to fetch: {len(todo):,}   "
          f"(~{len(todo) * 0.55 / 60:.0f} min at 109 req/min)", flush=True)
    errors = 0
    with open(CACHE, "a") as out:
        for i, cn in enumerate(todo, 1):
            items = ch_officers(cn, api_key)
            time.sleep(0.55)  # 0.55s ~= 109 req/min, under CH's 600/5min
            if items is None:
                errors += 1
                continue
            out.write(json.dumps({"cn": cn, "items": items}) + "\n")
            out.flush()
            if i % 200 == 0:
                print(f"  [{i}/{len(todo)}] fetched, {errors} errors", flush=True)
    print(f"done. errors: {errors}")


# ── phase 3: analyse ────────────────────────────────────────────────────────────

def officer_ident(item: dict) -> tuple:
    """CH's two identity signals, neither of which graph_events carries.

    `links.officer.appointments` embeds a per-PERSON officer id (`/officers/{id}/...`);
    `date_of_birth` is published as {month, year}. Either alone distinguishes two humans
    who share a name.
    """
    link = ((item.get("links") or {}).get("officer") or {}).get("appointments") or ""
    m = re.search(r"/officers/([^/]+)/", link)
    oid = m.group(1) if m else None
    dob = item.get("date_of_birth") or {}
    dob_t = (dob.get("year"), dob.get("month")) if dob else None
    return oid, dob_t


def existing_officer_companies(slugs: set[str]) -> dict[str, set[str]]:
    """For each colliding slug, the company numbers its existing node already directs.

    Needed because `graph_events` stores no officer_id and no DOB, so the only way to
    decide whether an existing `director:{slug}` node is the SAME human as an incoming
    appointment is to re-derive its identity from CH, via the companies it directs.
    """
    edges, = q("SELECT payload.src_id AS s, payload.dst_id AS d FROM graph_events "
               "WHERE kind='EdgeAdded' "
               "AND string::contains(<string>payload.weight_json,'\"type\":\"Directs\"');")
    out: dict[str, set[str]] = collections.defaultdict(set)
    want = {f"director:{s}" for s in slugs}
    for e in edges:
        a, b = e.get("s"), e.get("d")
        for ent, off in ((a, b), (b, a)):
            if off in want and isinstance(ent, str) and ent.startswith("GB-COH:"):
                out[off.split(":", 1)[1]].add(ent.split(":", 1)[1])
    return out


def dob_suffix(ident) -> str | None:
    """The established disambiguator: `-b{YYYYMM}`, as minted by the 2026-08-11 split
    (`data/officer-split-plan.tsv`; 207 such nodes are live). Not a new key format."""
    _oid, dob = ident
    if not dob or not dob[0] or not dob[1]:
        return None
    return f"-b{dob[0]:04d}{dob[1]:02d}"


def analyse(targets: list[str]) -> None:
    cache = load_cache()
    print("loading existing officer nodes from graph_events ...", flush=True)
    nodes, = q("SELECT payload.entity_id AS eid, payload.weight_json AS w FROM graph_events "
               "WHERE kind IN ['NodeAdded','NodeUpdated'] "
               "AND string::contains(<string>payload.weight_json,'\"type\":\"Officer\"');")
    existing = {r["eid"] for r in nodes if r.get("eid")}
    print(f"  existing officer nodes: {len(existing):,}")

    # slug -> identities we would newly write under it
    slug_all([o.get("name") for cn in targets for o in (cache.get(cn) or []) if o.get("name")])

    new_by_slug: dict[str, set] = collections.defaultdict(set)
    new_names: dict[str, set] = collections.defaultdict(set)
    n_appt = n_companies = 0
    for cn in targets:
        items = cache.get(cn)
        if not items:
            continue
        active = [o for o in items if not o.get("resigned_on")]
        if active:
            n_companies += 1
        for o in active:
            name = o.get("name")
            if not name:
                continue
            s = applicant_slug(name)
            if not s:
                continue
            n_appt += 1
            new_by_slug[s].add(officer_ident(o))
            new_names[s].add(name)

    collide = {s for s in new_by_slug if f"director:{s}" in existing}
    # A slug fuses distinct humans if the identities under it disagree on officer_id
    # (or, absent that, on DOB). Unknown-vs-known is not counted as proof of difference.
    def distinct_humans(idents) -> int:
        oids = {o for o, _ in idents if o}
        dobs = {d for _, d in idents if d}
        return max(len(oids), len(dobs))

    self_fusing = {s: ids for s, ids in new_by_slug.items() if distinct_humans(ids) > 1}

    print(f"\n── what a --write would do ──")
    print(f"  target companies with active officers: {n_companies:,}")
    print(f"  officer appointments to write:         {n_appt:,}")
    print(f"  distinct director slugs:               {len(new_by_slug):,}")
    print(f"  slugs that are NEW nodes:              {len(new_by_slug) - len(collide):,}")
    print(f"  slugs that MERGE INTO an existing node:{len(collide):,}")
    print(f"\n── Open Item 22 risk, quantified ──")
    print(f"  slugs fusing >1 provably distinct human (within this batch): {len(self_fusing):,}")
    for s in sorted(self_fusing)[:15]:
        print(f"    director:{s:<40} {sorted(new_names[s])[:3]} "
              f"{distinct_humans(self_fusing[s])} humans")
    n_ident = sum(1 for ids in new_by_slug.values() for o, d in ids if o or d)
    print(f"\n  appointments carrying an officer_id or DOB: {n_ident:,} of {n_appt:,}")



# ── phase 4: plan ───────────────────────────────────────────────────────────────

def build_plan(targets: list[str], api_key: str | None) -> list[dict]:
    """Decide a key for every appointment, using evidence rather than the name alone.

    Rules, in order:
      1. Default to `director:{slug}` — the load-bearing "one human, one node" key.
      2. If a slug covers more than one provably distinct human (distinct CH officer_id,
         or distinct DOB), every appointment under it takes the established
         `-b{YYYYMM}` disambiguator instead. This is the same convention the 2026-08-11
         split minted, not a new one.
      3. A slug that would merge into an EXISTING node is only allowed to merge when the
         existing node is evidenced to be the same human. Its identity is re-derived by
         fetching CH officers for the companies it already directs, because graph_events
         stores neither officer_id nor DOB. Same officer_id -> merge (that cross-company
         join is exactly the Phoenix signal). Different -> disambiguate. No evidence ->
         disambiguate, which is the conservative direction: a missed true join costs a
         detection, a false merge costs a false accusation (T-23).
    """
    cache = load_cache()
    nodes, = q("SELECT payload.entity_id AS eid FROM graph_events "
               "WHERE kind IN ['NodeAdded','NodeUpdated'] "
               "AND string::contains(<string>payload.weight_json,'\"type\":\"Officer\"');")
    existing = {r["eid"] for r in nodes if r.get("eid")}

    slug_all([o.get("name") for cn in targets for o in (cache.get(cn) or []) if o.get("name")])

    # gather this batch's appointments
    appts = []   # (company_number, officer_item, slug, ident)
    for cn in targets:
        for o in (cache.get(cn) or []):
            if o.get("resigned_on"):
                continue
            name = o.get("name")
            if not name:
                continue
            sl = applicant_slug(name)
            if sl:
                appts.append((cn, o, sl, officer_ident(o)))

    by_slug: dict[str, set] = collections.defaultdict(set)
    for _cn, _o, sl, ident in appts:
        by_slug[sl].add(ident)

    def distinct(idents):
        oids = {o for o, _ in idents if o}
        dobs = {d for _, d in idents if d}
        return max(len(oids), len(dobs))

    ambiguous = {sl for sl, ids in by_slug.items() if distinct(ids) > 1}

    # resolve slugs that collide with an existing node
    colliding = {sl for sl in by_slug if f"director:{sl}" in existing}
    ex_companies = existing_officer_companies(colliding) if colliding else {}
    to_fetch = sorted({cn for cns in ex_companies.values() for cn in cns} - set(cache))
    if to_fetch and api_key:
        print(f"resolving {len(colliding)} colliding slugs: fetching "
              f"{len(to_fetch)} companies the existing nodes direct ...", flush=True)
        with open(CACHE, "a") as out:
            for cn in to_fetch:
                items = ch_officers(cn, api_key)
                time.sleep(0.55)
                if items is not None:
                    out.write(json.dumps({"cn": cn, "items": items}) + "\n")
                    out.flush()
        cache = load_cache()
    elif to_fetch:
        print(f"warning: {len(to_fetch)} companies unresolved (no API key) — "
              f"their slugs will be disambiguated conservatively", flush=True)

    # identities CH attributes to each existing colliding node
    existing_ident: dict[str, set] = collections.defaultdict(set)
    for sl, cns in ex_companies.items():
        for cn in cns:
            for o in (cache.get(cn) or []):
                if o.get("name") and applicant_slug(o["name"]) == sl:
                    existing_ident[sl].add(officer_ident(o))

    plan, stats, skipped = [], collections.Counter(), []
    for cn, o, sl, ident in appts:
        suffix, why = "", "unambiguous"
        if sl in ambiguous:
            suffix, why = dob_suffix(ident) or "", "ambiguous-in-batch"
            if not suffix:
                # Rule 4: never write a key we already know fuses distinct humans.
                # CH publishes no DOB for secretaries and corporate officers, so a
                # disambiguator does not exist for these. A skipped appointment costs
                # one edge; a fused one costs a false accusation (T-23) and, like
                # `director:unknown-applicant` did to AddressCluster, a hub node that
                # inflates every motif it touches.
                skipped.append((cn, o.get("name"), o.get("officer_role"), sl))
                stats["SKIPPED-would-fuse-no-dob"] += 1
                continue
        elif sl in colliding:
            ex = existing_ident.get(sl, set())
            ex_oids = {oid for oid, _ in ex if oid}
            my_oid = ident[0]
            if my_oid and ex_oids and my_oid in ex_oids:
                why = "merge-same-officer-id"
            elif my_oid and ex_oids:
                suffix = dob_suffix(ident) or ""
                if not suffix:
                    skipped.append((cn, o.get("name"), o.get("officer_role"), sl))
                    stats["SKIPPED-would-fuse-no-dob"] += 1
                    continue
                why = "split-different-officer-id"
            else:
                suffix = dob_suffix(ident) or ""
                if not suffix:
                    skipped.append((cn, o.get("name"), o.get("officer_role"), sl))
                    stats["SKIPPED-would-fuse-no-dob"] += 1
                    continue
                why = "collide-no-evidence"
        stats[why] += 1
        plan.append({"cn": cn, "officer": o, "key": f"director:{sl}{suffix}", "why": why})

    print("\n── write plan ──")
    for k, v in sorted(stats.items()):
        print(f"  {k:<32} {v:>6,}")
    keys = {p["key"] for p in plan}
    print(f"\n  appointments: {len(plan):,}   distinct officer nodes: {len(keys):,}")
    print(f"  merging into an existing node: "
          f"{sum(1 for k in keys if k in existing):,}")
    print(f"  disambiguated with -b{{YYYYMM}}: "
          f"{sum(1 for p in plan if p['why'].startswith(('ambiguous-in-batch','split-','collide-no-evidence')) and p['key'].count('-b')):,}")
    print(f"  skipped rather than fused: {len(skipped):,}")
    if skipped:
        out = DATA / "officer-coverage-skipped.tsv"
        with open(out, "w") as f:
            f.write("company_number\tname\trole\tslug\n")
            for row in skipped:
                f.write("\t".join(str(x) for x in row) + "\n")
        print(f"  -> {out}")
    assert not any(p["key"].endswith(":") for p in plan)
    return plan



# ── phase 5: write ──────────────────────────────────────────────────────────────

EVENT_PREFIX = "cf-officer-cov"


def write(plan: list[dict], dry_run: bool) -> None:
    """Append Officer + Directs events for the planned appointments.

    Ordering: every NodeAdded is written before any EdgeAdded. Replay and projection
    page with `ORDER BY id` (persistence/scan.rs, projector.rs:464) over SurrealDB's
    monotonic generated ids, so insertion order IS event order — an edge inserted before
    its node would dangle, which the graph already carries 53 examples of.

    Idempotency is by `event_id` lookup, not by a deterministic record id: `graph_events`
    has no unique index on `event_id` (db.rs), and forcing our own record ids would put
    these rows at arbitrary positions in that `ORDER BY id` sequence.
    """
    existing_ids = set()
    rows, = q(f"SELECT event_id FROM graph_events "
              f"WHERE string::starts_with(event_id, '{EVENT_PREFIX}');")
    existing_ids = {r["event_id"] for r in rows if r.get("event_id")}
    if existing_ids:
        print(f"  {len(existing_ids):,} events from a previous run already present — skipping those")

    nodes, edges, seen_node = [], [], set()
    for item in plan:
        o, key, cn = item["officer"], item["key"], item["cn"]
        role = o.get("officer_role") or "director"
        if key not in seen_node:
            seen_node.add(key)
            w = {"type": "Officer", "name": o.get("name"), "officer_role": role}
            if o.get("appointed_on"):
                w["appointed_on"] = o["appointed_on"]
            if o.get("resigned_on"):
                w["resigned_on"] = o["resigned_on"]
            # Company scope lives in the weight, not the key (PR #38): the key is
            # `director:{slug}` so one human is one node, so anything scoping an officer
            # to a company reads OfficerData::company_number.
            w["company_number"] = cn
            nodes.append({
                "event_id": f"{EVENT_PREFIX}-node-{key}",
                "kind": "NodeAdded",
                "payload": {"entity_id": key,
                            "weight_json": json.dumps(w, separators=(",", ":"))},
            })
        ew = {"type": "Directs",
              "meta": {"created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                       "effective_from": None, "effective_to": None},
              "data": {"details": role, "officer_role": role}}
        edges.append({
            "event_id": f"{EVENT_PREFIX}-edge-GB-COH-{cn}-{key}",
            "kind": "EdgeAdded",
            "payload": {"src_id": f"GB-COH:{cn}", "dst_id": key,
                        "weight_json": json.dumps(ew, separators=(",", ":"))},
        })

    todo = [e for e in nodes + edges if e["event_id"] not in existing_ids]
    print(f"\n  officer nodes: {len(nodes):,}   directs edges: {len(edges):,}   "
          f"to write: {len(todo):,}")
    if dry_run:
        for e in todo[:10]:
            pl = e["payload"]
            print(f"    [dry-run] {e['kind']:<10} "
                  f"{pl.get('entity_id') or pl['src_id'] + ' -> ' + pl['dst_id']}")
        print(f"    ... ({len(todo):,} events; re-run with --write --apply to commit)")
        return

    ts = int(time.time() * 1000)
    written = 0
    for i in range(0, len(todo), 200):
        batch = [{"event_id": e["event_id"], "timestamp": ts, "kind": e["kind"],
                  "payload": e["payload"]} for e in todo[i:i + 200]]
        # Fail loud. A discarded write Result is how the ICIJ adapter logged 814k rows
        # written into an empty table for its entire life.
        res = TARGET.query("INSERT INTO graph_events " + json.dumps(batch), timeout=300)
        for r in res:
            if r.get("status") != "OK":
                raise SystemExit(f"write failed at batch {i}: {r}")
        written += len(batch)
        if (i // 200) % 5 == 0:
            print(f"    {written:,}/{len(todo):,} written", flush=True)
    print(f"  done: {written:,} events written")


def main():
    global TARGET
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    surreal_target.add_analysis_arg(p)
    p.add_argument("--targets", action="store_true", help="compute + save the target list")
    p.add_argument("--fetch", action="store_true", help="fetch officers from CH into the cache")
    p.add_argument("--analyse", action="store_true", help="quantify the fusion risk; writes nothing")
    p.add_argument("--plan", action="store_true",
                   help="decide a key per appointment using CH evidence; writes nothing")
    p.add_argument("--write", action="store_true",
                   help="write the planned Officer + Directs events (dry-run unless --apply)")
    p.add_argument("--apply", action="store_true", help="actually commit the --write")
    p.add_argument("--api-key", default=os.environ.get("URBAN__ENRICHER__CH__API_KEY"))
    args = p.parse_args()

    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner())

    if args.targets or not TARGETS_FILE.exists():
        targets = compute_targets()
        TARGETS_FILE.write_text("\n".join(targets) + "\n")
        print(f"  wrote {TARGETS_FILE}")
    else:
        targets = [l.strip() for l in TARGETS_FILE.read_text().splitlines() if l.strip()]
        print(f"targets loaded from {TARGETS_FILE}: {len(targets):,}")

    if args.fetch:
        if not args.api_key:
            raise SystemExit("error: no CH REST key — set URBAN__ENRICHER__CH__API_KEY")
        fetch(targets, args.api_key)
    if args.analyse:
        analyse(targets)
    if args.plan or args.write:
        plan = build_plan(targets, args.api_key)
    if args.write:
        write(plan, dry_run=not args.apply)


if __name__ == "__main__":
    main()
