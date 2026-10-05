#!/usr/bin/env python3
"""Where do Phoenix candidates die? Measured against graph_events — the source the
detector's in-memory graph is built from, NOT the projected tables (the projected
`directs` table holds 4,367 of the log's 22,942 Directs edges).

Mirrors crates/detector/src/motif/phoenix.rs:
  seed        = Active Entity
  award_date  = min(date_published) over Contract nodes adjacent to seed  [required]
  path        = seed -undirected-> Officer -undirected-> Entity{Dissolved|InLiquidation}(dt)
  emit        if |award_date - dt| <= 180 days
"""
import argparse, json, collections, datetime as dt

import surreal_target

parser = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
surreal_target.add_analysis_arg(parser)
TARGET = surreal_target.resolve(parser.parse_args())
TARGET.preflight()
print(TARGET.banner())


def q(sql):
    """Run SurrealQL and return one result list per statement, loudly."""
    out = TARGET.query(sql, timeout=1200)
    for r in out:
        if r.get("status") != "OK":
            raise SystemExit(f"query failed: {r}")
    return [r["result"] for r in out]

def parse_dt(s):
    if not s:
        return None
    try:
        return dt.datetime.fromisoformat(s.replace("Z", "+00:00"))
    except (ValueError, AttributeError):
        try:
            return dt.datetime.strptime(s, "%Y-%m-%d").replace(tzinfo=dt.timezone.utc)
        except (ValueError, TypeError):
            return None

# ── nodes: entity status/dissolution date, officer ids, contract dates ──────────
print("loading node events ...")
rows, = q("SELECT payload.entity_id AS eid, payload.weight_json AS w, kind FROM graph_events "
          "WHERE kind IN ['NodeAdded','NodeUpdated'];")
status, dissolved_dt, officers, cdate = {}, {}, set(), {}
corporate = set()
for r in rows:
    eid, w = r.get("eid"), r.get("w")
    if not eid or not w:
        continue
    o = json.loads(w)
    t = o.get("type")
    if t == "Entity":
        st = o.get("status")
        if isinstance(st, dict):
            status[eid] = st.get("type")
            if st.get("type") in ("Dissolved", "InLiquidation"):
                d = parse_dt(st.get("content"))
                if d:
                    dissolved_dt[eid] = d
                else:
                    dissolved_dt.pop(eid, None)
            else:
                dissolved_dt.pop(eid, None)
        elif isinstance(st, str):
            status[eid] = st
    elif t == "Officer":
        officers.add(eid)
        # Mirrors phoenix.rs: a `corporate-` role is a service provider (company
        # secretarial firms etc.), a hub node joining companies that share only a
        # supplier — excluded from the motif.
        if (o.get("officer_role") or "").startswith("corporate-"):
            corporate.add(eid)
    elif t == "Contract":
        cdate[eid] = parse_dt(o.get("date_published"))

print(f"  entities {len(status):,}  (active {sum(1 for v in status.values() if v=='Active'):,}, "
      f"dissolved/liq {sum(1 for v in status.values() if v in ('Dissolved','InLiquidation')):,}, "
      f"of which dated {len(dissolved_dt):,})")
print(f"  officers {len(officers):,} (corporate, excluded: {len(corporate):,})   contracts {len(cdate):,} "
      f"(dated {sum(1 for v in cdate.values() if v):,})")

# ── edges ───────────────────────────────────────────────────────────────────────
print("loading edge events ...")
edges, = q("SELECT payload.src_id AS s, payload.dst_id AS d, payload.weight_json AS w "
           "FROM graph_events WHERE kind='EdgeAdded';")
ent_off = collections.defaultdict(set)   # entity  -> officers (undirected)
off_ent = collections.defaultdict(set)   # officer -> entities (undirected)
ent_con = collections.defaultdict(set)   # entity  -> contracts
n_dir = n_aw = 0
for e in edges:
    s, d, w = e.get("s"), e.get("d"), e.get("w")
    if not s or not d or not w:
        continue
    t = json.loads(w).get("type")
    if t == "Directs":
        n_dir += 1
        for a, b in ((s, d), (d, s)):
            if b in officers and b not in corporate:
                ent_off[a].add(b)
                off_ent[b].add(a)
    elif t == "AwardedTo":
        n_aw += 1
        for a, b in ((s, d), (d, s)):
            if b in cdate:
                ent_con[a].add(b)
print(f"  Directs {n_dir:,}   AwardedTo {n_aw:,}")

# ── funnel ──────────────────────────────────────────────────────────────────────
stage = collections.Counter()
hits = []
for seed, st in status.items():
    if st != "Active":
        continue
    stage["1. active seed"] += 1
    partners = {(o, p) for o in ent_off.get(seed, ())
                       for p in off_ent.get(o, ())
                       if p != seed and p in dissolved_dt}
    if not partners:
        stage["2a. DIES: no shared officer reaching a dated dissolved entity"] += 1
        continue
    stage["2. reaches a dated dissolved entity via a shared officer"] += 1
    cons = ent_con.get(seed, ())
    if not cons:
        stage["3a. DIES: seed has no contract at all"] += 1
        continue
    dated = [cdate[c] for c in cons if cdate.get(c)]
    if not dated:
        stage["3b. DIES: has contracts, none carries a parseable date"] += 1
        continue
    stage["3. has a dated contract"] += 1
    award = min(dated)
    inwin = [(o, p) for (o, p) in partners if abs((award - dissolved_dt[p]).days) <= 180]
    if not inwin:
        stage["4a. DIES: outside the 180-day window"] += 1
        continue
    stage["4. WOULD FIRE"] += 1
    for o, p in inwin:
        hits.append((seed, o, p, award.date(), dissolved_dt[p].date()))

print("\n── Phoenix funnel ──")
for k in sorted(stage):
    print(f"  {k:<58} {stage[k]:>8,}")
print(f"\nwould-emit motifs: {len(hits)}")
for h in hits[:20]:
    print("   ", h)

# ── why do 86 of 92 have no contract? and where are the CF contracts' awardees? ──
print("\n── contract-side diagnosis ──")
cf = {c for c in cdate if c.startswith("CF:")}
pl = {c for c in cdate if c.startswith("planning:")}
print(f"  contracts: CF {len(cf):,} (dated {sum(1 for c in cf if cdate[c]):,})   "
      f"planning {len(pl):,} (dated {sum(1 for c in pl if cdate[c]):,})")

awardees = collections.defaultdict(set)   # contract -> entities awarded it
for e, cs in ent_con.items():
    for c in cs:
        awardees[c].add(e)
cf_aw = {e for c in cf for e in awardees.get(c, ())}
pl_aw = {e for c in pl for e in awardees.get(c, ())}
print(f"  distinct awardees: CF {len(cf_aw):,}   planning {len(pl_aw):,}")
print(f"  CF awardees that have ANY officer edge: {sum(1 for e in cf_aw if ent_off.get(e)):,}")
print(f"  CF awardees that are a known Entity:    {sum(1 for e in cf_aw if e in status):,}")
print(f"  planning awardees with ANY officer edge:{sum(1 for e in pl_aw if ent_off.get(e)):,}")

stage2 = {seed for seed, st in status.items() if st == "Active"
          and any(p != seed and p in dissolved_dt
                  for o in ent_off.get(seed, ()) for p in off_ent.get(o, ()))}
print(f"\n  stage-2 survivors: {len(stage2)}")
print(f"    with a CF contract:       {sum(1 for e in stage2 if ent_con.get(e, set()) & cf)}")
print(f"    with a planning contract: {sum(1 for e in stage2 if ent_con.get(e, set()) & pl)}")
print(f"    with no contract at all:  {sum(1 for e in stage2 if not ent_con.get(e))}")

# what would 100% CF date coverage buy?
undated_cf = {c for c in cf if not cdate[c]}
gain = {e for c in undated_cf for e in awardees.get(c, ()) if e in stage2}
print(f"\n  ceiling on Open Item 19 (a full CF re-ingest):")
print(f"    undated CF contracts: {len(undated_cf):,}")
print(f"    stage-2 survivors they would newly date: {len(gain)}")
