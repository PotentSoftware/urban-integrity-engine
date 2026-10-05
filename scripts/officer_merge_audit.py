#!/usr/bin/env python3
"""Quantify how many of the 531 name-only officer merges are genuinely one person.

WHY (planning open item 21). `scripts/rekey_officers_to_director_slug.py` re-keyed 22,807
officer nodes from the per-company `OFF:{company_number}-{NAME}` form to the cross-company
`director:{slug}` form, because Phoenix cannot see a serial director whose appointments are
five unconnected nodes. That re-key FUSED 531 groups on NAME ALONE — the events carry no
date of birth, so nothing in the graph establishes that two same-named officers are one
person. Phoenix's first (and so far only) alert rests on exactly such a fusion, so every
Phoenix output is unvalidated until this is measured. D-18 (exact-match-only) and
T-23-FalseAccusation both bear directly on it.

WHAT THIS MEASURES. Companies House REST exposes, per appointment:
  - `links.officer.appointments` -> `/officers/{officer_id}/appointments`, where officer_id
    is CH's own person identifier, shared across that person's appointments. This is the
    authoritative answer to "same person?" and is what the graph lacks.
  - `date_of_birth` as {month, year} (CH redacts the day) — a corroborating signal, and the
    tie-breaker when CH itself has fragmented one person across several officer_ids.

Corporate officers (`officer_role` starting `corporate-`) carry no DOB. A fusion of
"GETGROUND SECRETARY LIMITED" across 50 companies is a real single legal entity, so those
are classified separately rather than counted as person merges.

VERDICTS, per fused `director:{slug}` group:
  GENUINE      — every appointment maps to ONE CH officer_id.
  GENUINE_DOB  — several officer_ids but a single shared DOB; CH fragments a person whose
                 appointments were filed with differing details. Treated as one person.
  CORPORATE    — every appointment is a corporate officer with one canonical name.
  FALSE_MERGE  — two or more DISTINCT DATES OF BIRTH. Demonstrably different humans. Every
                 alert touching this node is unsafe.
  UNCERTAIN    — several officer_ids but no DOB to adjudicate. CH publishes DOB for
                 directors and NOT for secretaries, so a secretary fused across companies
                 cannot be confirmed or refuted here. Not counted as defensible: absence of
                 contradicting evidence is not evidence of identity (D-18).
  UNRESOLVED   — CH returned no matching appointment (company gone, name re-filed, etc).

READ-ONLY. Touches CH REST and the local cache only; writes nothing to SurrealDB.

Usage:
  python3 scripts/officer_merge_audit.py            # resume-safe; uses cache
  python3 scripts/officer_merge_audit.py --refresh  # ignore cache, refetch
"""
import os, sys, json, time, base64, argparse, urllib.request, urllib.error
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
COLLISIONS = os.path.join(ROOT, "data", "rekey-officer-collisions.tsv")
CACHE = os.path.join(ROOT, "data", "ch-officers-cache.jsonl")
OUT = os.path.join(ROOT, "data", "officer-merge-audit.tsv")
API = "https://api.company-information.service.gov.uk"

# CH REST allows 600 requests per 5-minute window = 2/s. Stay under it; a 429 storm costs
# more wall-clock than the delay does.
DELAY = 0.55


def applicant_slug(name):
    """Mirror of `ingestor::planning::mapper::applicant_slug` (the shim ch_backfill.py used).

    Kept in Python deliberately: this is an audit of what the re-key DID, so it must
    reproduce the key it produced, independent of the current Rust source.
    """
    s = "".join(c if c.isalnum() else "-" for c in (name or "").lower())
    return "-".join(p for p in s.split("-") if p)


class Ch:
    def __init__(self):
        key = os.environ.get("URBAN__ENRICHER__CH__API_KEY")
        if not key:
            raise SystemExit("URBAN__ENRICHER__CH__API_KEY not set (REST key, not streaming)")
        self.auth = "Basic " + base64.b64encode((key + ":").encode()).decode()
        self.calls = 0

    def get(self, path):
        req = urllib.request.Request(API + path, headers={"Authorization": self.auth})
        for attempt in range(6):
            try:
                with urllib.request.urlopen(req, timeout=45) as r:
                    self.calls += 1
                    time.sleep(DELAY)
                    return json.loads(r.read().decode())
            except urllib.error.HTTPError as e:
                if e.code == 404:
                    return None
                if e.code == 429:
                    wait = 30 * (attempt + 1)
                    print(f"    429 — backing off {wait}s", flush=True)
                    time.sleep(wait)
                    continue
                print(f"    HTTP {e.code} on {path}", flush=True)
                return None
            except Exception as e:
                wait = 5 * (attempt + 1)
                print(f"    {str(e)[:80]} — retry in {wait}s", flush=True)
                time.sleep(wait)
        return None

    def officers(self, cn):
        """All appointments on a company, following CH's pagination."""
        items, start = [], 0
        while True:
            d = self.get(f"/company/{cn}/officers?items_per_page=100&start_index={start}")
            if not d:
                break
            batch = d.get("items") or []
            items += batch
            start += len(batch)
            if not batch or start >= (d.get("total_results") or 0):
                break
        return items


def load_cache():
    cache = {}
    if os.path.exists(CACHE):
        with open(CACHE) as f:
            for line in f:
                try:
                    r = json.loads(line)
                    cache[r["cn"]] = r["items"]
                except Exception:
                    continue
    return cache


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--refresh", action="store_true")
    args = ap.parse_args()

    groups = {}          # director slug -> [company_number, ...]
    with open(COLLISIONS) as f:
        next(f)
        for line in f:
            new_id, _n, srcs = line.rstrip("\n").split("\t")
            cns = []
            for s in srcs.split(","):
                # OFF:{company_number}-{NAME_SLUG}
                cns.append(s.split(":", 1)[1].split("-", 1)[0])
            groups[new_id] = cns

    wanted = sorted({cn for cns in groups.values() for cn in cns})
    cache = {} if args.refresh else load_cache()
    todo = [cn for cn in wanted if cn not in cache]
    print(f"groups: {len(groups)}   companies: {len(wanted)}   cached: {len(wanted)-len(todo)}   to fetch: {len(todo)}")

    if todo:
        ch = Ch()
        with open(CACHE, "a") as f:
            for i, cn in enumerate(todo):
                items = ch.officers(cn)
                cache[cn] = items
                f.write(json.dumps({"cn": cn, "items": items}) + "\n")
                f.flush()
                if i % 50 == 0:
                    print(f"  [{i}/{len(todo)}] {cn} -> {len(items)} appointment(s)", flush=True)
        print(f"fetched {len(todo)} companies in {ch.calls} API calls\n")

    # Resolve each fused group against CH.
    rows, tally = [], defaultdict(int)
    person_false, corp_false = [], []
    for slug_id, cns in sorted(groups.items()):
        slug = slug_id.split(":", 1)[1]
        ids, dobs, roles, names, missing = set(), set(), set(), set(), 0
        for cn in cns:
            hits = [o for o in (cache.get(cn) or []) if applicant_slug(o.get("name")) == slug]
            if not hits:
                missing += 1
                continue
            for o in hits:
                link = ((o.get("links") or {}).get("officer") or {}).get("appointments") or ""
                oid = link.split("/officers/", 1)[1].split("/")[0] if "/officers/" in link else ""
                if oid:
                    ids.add(oid)
                d = o.get("date_of_birth") or {}
                if d.get("year"):
                    dobs.add(f"{d.get('year')}-{d.get('month'):02d}" if d.get("month") else str(d["year"]))
                roles.add(o.get("officer_role") or "")
                names.add(o.get("name") or "")

        corporate = bool(roles) and all(r.startswith("corporate-") for r in roles)
        if not ids and not dobs:
            verdict = "UNRESOLVED"
        elif corporate:
            verdict = "CORPORATE" if len(names) <= 1 else "FALSE_MERGE"
        elif len(ids) <= 1:
            verdict = "GENUINE"
        elif len(dobs) == 1:
            verdict = "GENUINE_DOB"
        elif len(dobs) == 0:
            # Distinct CH officer_ids but nothing to adjudicate on. Overwhelmingly
            # secretaries, for whom CH publishes no DOB.
            verdict = "UNCERTAIN"
        else:
            verdict = "FALSE_MERGE"

        tally[verdict] += 1
        if verdict == "FALSE_MERGE":
            (corp_false if corporate else person_false).append(
                (slug_id, len(cns), len(ids), sorted(dobs)))
        rows.append((slug_id, len(cns), verdict, len(ids), ";".join(sorted(dobs)),
                     ";".join(sorted(roles)), missing))

    with open(OUT, "w") as f:
        f.write("director_id\tappointments\tverdict\tdistinct_ch_officer_ids\tdobs\troles\tunmatched\n")
        for r in rows:
            f.write("\t".join(str(x) for x in r) + "\n")

    total = len(groups)
    print("=" * 72)
    print(f"{'verdict':<14}{'groups':>8}{'share':>9}")
    for v in ("GENUINE", "GENUINE_DOB", "CORPORATE", "FALSE_MERGE", "UNCERTAIN", "UNRESOLVED"):
        n = tally[v]
        print(f"{v:<14}{n:>8}{100.0*n/total:>8.1f}%")
    print("=" * 72)
    good = tally["GENUINE"] + tally["GENUINE_DOB"] + tally["CORPORATE"]
    bad = tally["FALSE_MERGE"]
    unk = tally["UNCERTAIN"] + tally["UNRESOLVED"]
    print(f"defensible merges : {good}/{total} ({100.0*good/total:.1f}%)")
    print(f"false merges      : {bad} ({100.0*bad/total:.1f}%) — DOB-proven distinct humans; "
          f"every alert touching these is unsafe")
    print(f"not adjudicable   : {unk} ({100.0*unk/total:.1f}%) — no DOB published; must not be "
          f"counted as defensible")
    print(f"\nwritten: {OUT}")

    if person_false:
        print(f"\nfalse person merges, largest first:")
        for slug_id, n, nids, dobs in sorted(person_false, key=lambda x: -x[1])[:15]:
            print(f"  {slug_id:<44} {n:>3} appointments  {nids} people  dobs={','.join(dobs)}")


if __name__ == "__main__":
    main()
