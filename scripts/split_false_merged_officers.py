#!/usr/bin/env python3
"""Split the officer nodes that the name-only re-key wrongly fused into one person.

WHY (planning open item 21). `scripts/rekey_officers_to_director_slug.py` re-keyed officers
to `director:{slug}` so that one human is one node — necessary, because Phoenix's motif
needs a single officer node shared by two companies. But the graph carries no date of
birth, so the re-key fused on NAME ALONE and `data/rekey-officer-collisions.tsv` recorded
531 fusions it could not adjudicate.

`scripts/officer_merge_audit.py` then adjudicated all 531 against Companies House REST,
which publishes the two things the graph lacks — a per-person `officer_id` and a
`{month, year}` date of birth. Result: 437 defensible, and **94 not**:

  FALSE_MERGE  77   two or more DISTINCT DATES OF BIRTH — demonstrably different humans
  UNCERTAIN    16   several CH officer_ids, no DOB published (secretaries)
  UNRESOLVED    1   no CH appointment matched at all

Among the 77 is the node carrying the project's only `PhoenixLoop` alert at the time:
its two appointments carry different dates of birth at two different companies. That
alert is a false accusation of two unconnected people, which is `T-23-FalseAccusation`, the
gravest failure mode this system has.

WHAT THIS DOES. For each of the 94 groups only, it splits the fused node back apart, using
the strongest evidence available for that group:

  DOB       `director:{slug}-b{YYYYMM}`  — preferred. Two appointments share a node iff CH
                                           publishes the same DOB for both.
  officer   `director:{slug}-o{id}`      — when CH publishes no DOB (secretaries). One CH
                                           officer_id per node: under D-18 an unproven
                                           merge must not be made, so these stay separate.
  company   `director:{slug}-c{number}`  — last resort, when CH matched no appointment.
                                           Reverts that appointment to per-company scope.

The 437 adjudicated-good groups are NOT touched. Neither is any officer outside the 531.

NOTE THIS IS DELIBERATELY LOSSY IN ONE DIRECTION. Splitting can only ever REMOVE motifs, so
it cannot manufacture an accusation; leaving the fusions in place can only ever ADD false
ones. That asymmetry is why this is safe to run and unsafe to skip.

SAFETY. Rewrites `graph_events`, the append-only source of truth — a data correction, not a
routine write. Dry-run by default. Every write is verified to return a row, because
SurrealDB reports `status: OK` with an empty result for an UPDATE matching nothing. A
`project` re-run is required afterwards, then a bake to re-derive alerts.

Usage:
  python3 scripts/split_false_merged_officers.py            # dry run + plan
  python3 scripts/split_false_merged_officers.py --apply
"""
import os, sys, json, argparse, collections

sys.path.insert(0, 'scripts')
import analyst_probe as ap

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
AUDIT = os.path.join(ROOT, "data", "officer-merge-audit.tsv")
COLLISIONS = os.path.join(ROOT, "data", "rekey-officer-collisions.tsv")
CACHE = os.path.join(ROOT, "data", "ch-officers-cache.jsonl")
PLAN = os.path.join(ROOT, "data", "officer-split-plan.tsv")

SPLIT_VERDICTS = {"FALSE_MERGE", "UNCERTAIN", "UNRESOLVED"}


def applicant_slug(name):
    """Mirror of `ingestor::planning::mapper::applicant_slug`, as used by the re-key."""
    s = "".join(c if c.isalnum() else "-" for c in (name or "").lower())
    return "-".join(p for p in s.split("-") if p)


def sanitise(x):
    """Keep the key charset the rest of the graph uses: [a-z0-9-]."""
    return "".join(c for c in (x or "") if c.isalnum()).lower()


def load_audit():
    v = {}
    with open(AUDIT) as f:
        next(f)
        for line in f:
            p = line.rstrip("\n").split("\t")
            v[p[0]] = p[2]
    return v


def load_groups():
    g = {}
    with open(COLLISIONS) as f:
        next(f)
        for line in f:
            new_id, _n, srcs = line.rstrip("\n").split("\t")
            g[new_id] = [s.split(":", 1)[1].split("-", 1)[0] for s in srcs.split(",")]
    return g


def load_cache():
    c = {}
    with open(CACHE) as f:
        for line in f:
            try:
                r = json.loads(line)
                c[r["cn"]] = r["items"]
            except Exception:
                continue
    return c


def page(db, fields, where):
    out, start = [], 0
    while True:
        rows = db.query(f"SELECT {fields} FROM graph_events {where} ORDER BY id LIMIT 5000 START {start};")
        if not rows:
            break
        out.extend(rows)
        start += len(rows)
    return out


def main():
    ap_ = argparse.ArgumentParser()
    ap_.add_argument("--apply", action="store_true")
    args = ap_.parse_args()

    verdicts, groups, cache = load_audit(), load_groups(), load_cache()
    targets = {k: v for k, v in verdicts.items() if v in SPLIT_VERDICTS}
    print(f"groups audited: {len(verdicts)}   to split: {len(targets)} "
          f"({collections.Counter(targets.values())})")

    # Resolve every source APPOINTMENT to a discriminated new key. The unit is the
    # appointment, not the company: `ch_backfill.py` emitted one node and one edge event per
    # appointment, and 14 of these companies carry several same-named appointments — some of
    # them genuinely different people (one company has two identically named directors with
    # different dates of birth, whom even the ORIGINAL per-company key fused).
    appts = collections.defaultdict(list)      # slug_id -> [appointment, ...]
    basis = collections.Counter()
    for slug_id in sorted(targets):
        slug = slug_id.split(":", 1)[1]
        for cn in groups[slug_id]:
            hits = [o for o in (cache.get(cn) or []) if applicant_slug(o.get("name")) == slug]
            if not hits:
                appts[slug_id].append({"cn": cn, "key": f"{slug_id}-c{cn}",
                                       "appointed_on": None, "role": None})
                basis["company (no CH match)"] += 1
                continue
            for o in hits:
                d = o.get("date_of_birth") or {}
                if d.get("year") and d.get("month"):
                    key = f"{slug_id}-b{d['year']}{d['month']:02d}"
                    basis["date of birth"] += 1
                else:
                    link = ((o.get("links") or {}).get("officer") or {}).get("appointments") or ""
                    oid = link.split("/officers/", 1)[1].split("/")[0] if "/officers/" in link else ""
                    if oid:
                        key = f"{slug_id}-o{sanitise(oid)}"
                        basis["CH officer_id"] += 1
                    else:
                        key = f"{slug_id}-c{cn}"
                        basis["company (no officer_id)"] += 1
                appts[slug_id].append({"cn": cn, "key": key,
                                       "appointed_on": o.get("appointed_on"),
                                       "role": o.get("officer_role")})

    print("discriminator used per appointment:", dict(basis))
    newkeys = collections.defaultdict(set)
    for slug_id, lst in appts.items():
        for a in lst:
            newkeys[slug_id].add(a["key"])
    collapsed = [s for s, ks in newkeys.items() if len(ks) == 1]
    print(f"groups that split into >1 node: {len(newkeys)-len(collapsed)}   "
          f"groups that stay one node: {len(collapsed)} (all their appointments share evidence)")

    db = ap.SurrealClient()
    print(f"\nanalysis: {db.ns}/{db.db}   mode: {'APPLY' if args.apply else 'DRY RUN'}")

    ids = sorted(targets)
    inlist = ", ".join(json.dumps(i) for i in ids)
    edges = page(db, "id, payload.src_id AS src, payload.dst_id AS dst",
                 f"WHERE kind='EdgeAdded' AND payload.dst_id IN [{inlist}]")
    nodes = page(db, "id, payload.entity_id AS eid, payload.weight_json AS w",
                 f"WHERE kind='NodeAdded' AND payload.entity_id IN [{inlist}]")
    print(f"affected events: {len(nodes)} NodeAdded, {len(edges)} EdgeAdded")

    # Edges carry the company in src_id, so each maps to one of that company's appointments.
    # Where a company holds several (the same-named case), the edges are handed out one per
    # appointment — which is correct: the company really does have both people as officers.
    edge_updates, edge_miss = [], 0
    used_edge = collections.defaultdict(set)
    for e in edges:
        cn = (e["src"] or "").split(":", 1)[-1]
        pool = [a for a in appts.get(e["dst"], []) if a["cn"] == cn]
        u = used_edge[(e["dst"], cn)]
        pick = next((i for i in range(len(pool)) if i not in u), None)
        if pick is None:
            # More edge events than CH appointments — a resigned officer CH no longer lists.
            # Falling back to a company-scoped key keeps the edge rather than dropping it,
            # and cannot merge it with anyone.
            if pool:
                edge_updates.append((str(e["id"]).split(":", 1)[1].strip("`"), pool[0]["key"]))
            else:
                edge_updates.append((str(e["id"]).split(":", 1)[1].strip("`"),
                                     f"{e['dst']}-c{cn}"))
            edge_miss += 1
            continue
        u.add(pick)
        edge_updates.append((str(e["id"]).split(":", 1)[1].strip("`"), pool[pick]["key"]))

    # Node events do NOT record their company — the re-key left record ids random — so match
    # each to a CH appointment on appointed_on + role, greedily, one appointment per event.
    by_group = collections.defaultdict(list)
    for n in nodes:
        by_group[n["eid"]].append(n)
    node_updates, node_miss = [], 0
    for slug_id, evs in by_group.items():
        pool = appts.get(slug_id, [])
        used = set()
        for n in evs:
            try:
                w = json.loads(n["w"] or "{}")
            except Exception:
                w = {}
            want = (w.get("appointed_on"), w.get("officer_role"))
            pick = next((i for i, p in enumerate(pool)
                         if i not in used and (p["appointed_on"], p["role"]) == want), None)
            if pick is None:      # appointment details changed at CH since the backfill
                pick = next((i for i in range(len(pool)) if i not in used), None)
            if pick is None:
                # Every appointment already claimed. Leaving this event on the fused key
                # would re-create the merge, so scope it to its own node instead.
                node_updates.append((str(n["id"]).split(":", 1)[1].strip("`"),
                                     f"{slug_id}-unmatched"))
                node_miss += 1
                continue
            used.add(pick)
            node_updates.append((str(n["id"]).split(":", 1)[1].strip("`"), pool[pick]["key"]))

    print(f"planned updates: {len(node_updates)} node, {len(edge_updates)} edge   "
          f"(unmapped: {node_miss} node, {edge_miss} edge)")

    with open(PLAN, "w") as f:
        f.write("old_id\tnew_id\tappointments\n")
        for slug_id in sorted(newkeys):
            for k in sorted(newkeys[slug_id]):
                n = sum(1 for a in appts[slug_id] if a["key"] == k)
                f.write(f"{slug_id}\t{k}\t{n}\n")
    print(f"plan written: {PLAN}")

    if not args.apply:
        print("\nDRY RUN — no writes. Rerun with --apply.")
        return

    def run(updates, field):
        done = 0
        for i in range(0, len(updates), 50):
            chunk = updates[i:i + 50]
            sql, params = [], {}
            for j, (rid, val) in enumerate(chunk):
                sql.append(f"UPDATE type::record('graph_events', $k{j}) SET payload.{field} = $v{j};")
                params[f"k{j}"], params[f"v{j}"] = rid, val
            blocks = db.query_multi_bound(''.join(sql), params)
            bad = [b for b in blocks if b.get('status') != 'OK']
            if bad:
                raise SystemExit(f"{len(bad)} rejected, first: {str(bad[0])[:220]}")
            empty = [n for n, b in enumerate(blocks) if not b.get('result')]
            if empty:
                raise SystemExit(
                    f"{len(empty)} UPDATE(s) matched NO ROWS despite status OK — first id "
                    f"{chunk[empty[0]][0]!r}. Refusing to report these as writes.")
            done += len(chunk)
        return done

    print("\napplying…")
    n_ok = run(node_updates, "entity_id")
    e_ok = run(edge_updates, "dst_id")
    print(f"updated {n_ok} node event(s), {e_ok} edge event(s)")

    left = db.query("SELECT count() FROM graph_events "
                    f"WHERE payload.entity_id IN [{inlist}] OR payload.dst_id IN [{inlist}] GROUP ALL;")
    print(f"events still on a fused key: {left}")
    print("Next: `project` to rebuild native tables, then a bake to re-derive alerts.")


if __name__ == "__main__":
    main()
