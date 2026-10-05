#!/usr/bin/env python3
"""Clear the withheld applicant->company matches with a SECOND corroborating signal.

WHY (planning open item 27). `scripts/resolve_planning_applicants_to_ch.py` found 568
exact-name matches for planning applicants asserting corporate form. It applied 117 and
**withheld 454** with outcome `review_offsite`, because a UK company name is unique but a
name match does not prove the applicant IS that company: the Anglesey applicant `LDC Ltd`,
on a Menai Bridge boatyard application, matched `01705327 LDC LIMITED` — a landscaping
company in Guildford, 250 miles away. Short generic names (`A.F.M. Ltd`, `A B C & D Ltd`)
match a same-named company anywhere in Britain. `planning_identity.rs` auto-links a
full-name match with no review and writes a real `Directs` edge, so a wrong pick becomes a
`T-23-FalseAccusation` in the graph.

The withheld set is NOT wrong by construction — a distant registered office is normal for a
national firm (Stena Line Ports, Premier Inn legitimately build here from distant head
offices). Open Item 27's instruction is therefore to add a second corroborating signal
rather than relax the locality gate. This script supplies it.

THE SIGNAL. Registered-office locality asks where the company's post goes. This asks where
its PEOPLE are: `/company/{n}/officers`, and whether any serving officer's address falls in
the postcode areas the analysis's own configured wards imply. That distinguishes exactly
the case that motivated the gate — a Guildford landscaper has no Anglesey officer, a local
firm registered through an accountant in Chester does.

Two more facts are collected and REPORTED but never promote anything on their own:

  officer_known   a serving officer already exists in this analysis as a `director:` node,
                  by exact slug in either name ordering. Real evidence, but a common name
                  ("Paul Smith") makes it worthless, so the count of analysis nodes sharing
                  that slug is printed alongside and a human decides.
  sic_codes       from `/company/{n}`. A construction/real-estate SIC makes a planning
                  applicant plausible; it is a statement about KIND, not identity, and
                  thousands of builders exist nationwide. Never a promoter.

Promoting on officer locality alone, with the other two as review aids, is deliberate: the
promoting signal has to be one an impostor cannot satisfy by being in the same trade.

EXACT-MATCH-ONLY (2026-06-13). No score, no threshold, no phonetics, no nearest match. The
locality test is postcode-area set membership; the officer test is string equality between
slugs produced by the shipped `normalise_name`.

UNKNOWN IS NOT NEGATIVE. A company whose officers CH would not return (transport failure,
404) is reported `unknown_officers` and stays withheld. It is never counted as "no local
officer", which would withhold the right rows for the wrong reason.

WHAT --apply WRITES. Only `GB-COH:` entity events into `graph_events`, exactly as the
parent script does for its `new` outcome — CANDIDATES ONLY. No `identity_links` row and no
`Directs` edge is created here; `enrich --source planning_identity` makes the join with its
own exact-name and ambiguity rules. Dry-run by default.

Usage:
  python3 scripts/corroborate_offsite_applicant_matches.py
  python3 scripts/corroborate_offsite_applicant_matches.py --limit 25
  python3 scripts/corroborate_offsite_applicant_matches.py --apply
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import analyst_probe as ap
import resolve_planning_applicants_to_ch as base

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MATCHES = os.path.join(ROOT, "data", "planning-applicant-ch-matches.tsv")
OUT = os.path.join(ROOT, "data", "planning-applicant-ch-corroboration.tsv")
CACHE = os.path.join(ROOT, "data", "ch-company-detail-cache.jsonl")


def normalise_name(name: str) -> str:
    """Mirror of `core::stream::normalise::normalise_name`.

    The parent script shells out to the SHIPPED binary for every name it MATCHES on, and so
    does this one — see `base.normalise_all`. This local copy is used only for the officer
    slugs, which decide nothing on their own (see the module docstring): they populate a
    review column, never a promotion.
    """
    lower = (name or "").lower()
    elided = "".join(c for c in lower if c not in "'’-")
    spaced = "".join(c if (c.isalnum() or c.isspace()) else " " for c in elided)
    s = spaced.strip()
    for suffix in ("limited", "ltd", "plc", "llp", "llc"):
        if s == suffix:
            s = ""
        elif s.endswith(" " + suffix):
            s = s[: -(len(suffix) + 1)]
    return " ".join(s.split())


def officer_name_slugs(name: str) -> list:
    """Mirror of `ch_psc::officer_name_slugs` — as-written, then forename-first."""
    out = []
    as_written = normalise_name(name).replace(" ", "-")
    if as_written:
        out.append(as_written)
    if "," in (name or ""):
        surname, forenames = name.split(",", 1)
        surname, forenames = surname.strip(), forenames.strip()
        if surname and forenames:
            reordered = normalise_name(f"{forenames} {surname}").replace(" ", "-")
            if reordered and reordered not in out:
                out.append(reordered)
    return out


def postcode_area(text: str):
    m = base.POSTCODE.search((text or "").upper())
    return m.group(1) if m else None


def address_areas(address: dict) -> set:
    """Postcode areas asserted by a CH address object.

    `postal_code` is authoritative; the other lines are searched too because CH addresses
    are operator-entered and a postcode sometimes lands in `address_line_2` or `locality`.
    """
    areas = set()
    if not isinstance(address, dict):
        return areas
    for key in ("postal_code", "address_line_1", "address_line_2", "locality", "region"):
        area = postcode_area(address.get(key) or "")
        if area:
            areas.add(area)
    return areas


def load_withheld() -> list:
    if not os.path.exists(MATCHES):
        sys.exit(
            f"{MATCHES} not found — run scripts/resolve_planning_applicants_to_ch.py first"
        )
    rows = []
    with open(MATCHES) as f:
        header = next(f).rstrip("\n").split("\t")
        for line in f:
            parts = line.rstrip("\n").split("\t")
            row = dict(zip(header, parts))
            if row.get("outcome") == "review_offsite" and row.get("company_number"):
                rows.append(row)
    return rows


def load_cache() -> dict:
    cache = {}
    if os.path.exists(CACHE):
        with open(CACHE) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except Exception:
                    continue
                cache[r["n"]] = r
    return cache


def analysis_context(db):
    """The analysis's operating postcode areas, its `director:` slugs, and its GB-COH set.

    Areas are DERIVED from the `location:` nodes the configured wards produced — never
    hardcoded geography — so the set grows by itself when more wards are configured and
    this stays correct when the system goes UK-wide.
    """
    areas = set()
    for r in base.page(
        db,
        "payload.weight_json AS w",
        "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id ?? '', 'location:')",
    ):
        try:
            name = (json.loads(r["w"] or "{}").get("name") or "")
        except Exception:
            continue
        area = postcode_area(name)
        if area:
            areas.add(area)

    directors = collections.Counter()
    for r in base.page(
        db,
        "payload.entity_id AS eid",
        "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id ?? '', 'director:')",
    ):
        if r["eid"]:
            directors[r["eid"][len("director:"):]] += 1

    have = set()
    for r in base.page(
        db,
        "payload.entity_id AS eid",
        "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id ?? '', 'GB-COH:')",
    ):
        if r["eid"]:
            have.add(r["eid"])
    return areas, directors, have


def assess(detail, areas, directors):
    """Classify one candidate company. Returns (verdict, local_areas, known, sic)."""
    officers = detail.get("officers")
    profile = detail.get("profile") or {}
    sic = ",".join(profile.get("sic_codes") or [])

    if officers is None:
        return "unknown_officers", "", "", sic

    local, known = set(), []
    for officer in officers:
        if officer.get("resigned_on"):
            continue
        found = address_areas(officer.get("address") or {})
        local |= found & areas
        for slug in officer_name_slugs(officer.get("name") or ""):
            if slug in directors:
                known.append(f"{slug}x{directors[slug]}")

    known_col = ";".join(sorted(set(known)))
    if local:
        return "corroborated_officer_local", ",".join(sorted(local)), known_col, sic
    return "uncorroborated", "", known_col, sic


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--apply", action="store_true",
                        help="write the corroborated companies to graph_events as candidates")
    parser.add_argument("--limit", type=int, default=0,
                        help="only assess the first N withheld matches (sampling)")
    args = parser.parse_args()

    db = ap.SurrealClient()
    print(f"analysis: {db.ns}/{db.db}   mode: {'APPLY' if args.apply else 'DRY RUN'}")

    withheld = load_withheld()
    areas, directors, have = analysis_context(db)
    print(f"withheld matches: {len(withheld)}   "
          f"operating postcode areas: {sorted(areas) if areas else '(none)'}   "
          f"director slugs in analysis: {len(directors)}")
    if not areas:
        sys.exit("error: no postcode areas derived from this analysis's locations — the "
                 "locality test would pass nothing and a promotion would be meaningless")

    items = withheld[: args.limit] if args.limit else withheld
    cache = load_cache()
    ch = None
    tally = collections.Counter()
    results = []
    promote = {}

    fh = open(CACHE, "a")
    try:
        for i, row in enumerate(items):
            number = row["company_number"]
            detail = cache.get(number)
            if detail is None:
                if ch is None:
                    ch = base.Ch()
                officers = ch.get(f"/company/{number}/officers?items_per_page=100")
                profile = ch.get(f"/company/{number}")
                detail = {
                    "n": number,
                    "officers": (officers or {}).get("items") if officers else None,
                    "profile": profile or {},
                }
                fh.write(json.dumps(detail) + "\n")
                fh.flush()
            if i and i % 50 == 0:
                print(f"  [{i}/{len(items)}] …", flush=True)

            verdict, local, known, sic = assess(detail, areas, directors)
            tally[verdict] += 1
            if known:
                tally["  (also: an officer already in this analysis)"] += 1
            results.append((row, verdict, local, known, sic))
            if verdict == "corroborated_officer_local":
                gid = f"GB-COH:{number}"
                if gid in have:
                    tally["corroborated but already in the graph"] += 1
                else:
                    promote[gid] = detail.get("profile") or {}
    finally:
        fh.close()

    with open(OUT, "w") as f:
        f.write("applicant_id\tapplicant_name\tcompany_number\tcompany_name\tverdict\t"
                "local_officer_areas\tofficers_known_here\tsic_codes\n")
        for row, verdict, local, known, sic in results:
            f.write(f"{row['applicant_id']}\t{row['applicant_name']}\t{row['company_number']}\t"
                    f"{row.get('company_name','')}\t{verdict}\t{local}\t{known}\t{sic}\n")

    print(f"\nassessed {len(items)} withheld match(es):")
    for key, count in tally.most_common():
        print(f"  {count:>6}  {key}")
    print(f"\nwritten: {os.path.relpath(OUT, ROOT)}")
    print(f"companies to promote to candidates: {len(promote)}")

    if not args.apply:
        print("\nDRY RUN — no writes. Rerun with --apply.")
        return 0
    if not promote:
        print("nothing to write")
        return 0

    print("\nwriting GB-COH entities to graph_events…")
    ts = int(time.time() * 1000)
    keyed = sorted(promote.items())
    done = 0
    for i in range(0, len(keyed), 50):
        chunk = keyed[i : i + 50]
        sql, params = [], {}
        for j, (gid, profile) in enumerate(chunk):
            number = gid.split(":", 1)[1]
            dissolved = profile.get("company_status") == "dissolved"
            status = {"type": "Dissolved" if dissolved else "Active"}
            if dissolved:
                d = (profile.get("date_of_cessation") or "")[:10]
                # Phoenix cannot window an undated cessation; epoch is the mapper's own
                # sentinel for "dissolved, date unknown".
                status["content"] = f"{d}T00:00:00Z" if d else "1970-01-01T00:00:00Z"
            weight = {
                "type": "Entity",
                "name": profile.get("company_name") or "",
                "company_number": number,
                "status": status,
            }
            if profile.get("date_of_creation"):
                weight["incorporation_date"] = profile["date_of_creation"][:10]
            sql.append(
                f"UPSERT type::record('graph_events', $k{j}) CONTENT {{"
                f'"event_id": $e{j}, "timestamp": {ts}, "kind": "NodeAdded", '
                f'"payload": {{"entity_id": $i{j}, "weight_json": $v{j}}}}};'
            )
            params[f"k{j}"] = f"NodeAdded_applicant_resolve_{number}"
            params[f"e{j}"] = f"applicant-resolve-{number}"
            params[f"i{j}"] = gid
            params[f"v{j}"] = json.dumps(weight, separators=(",", ":"))
        blocks = db.query_multi_bound("".join(sql), params)
        bad = [b for b in blocks if b.get("status") != "OK"]
        if bad:
            sys.exit(f"{len(bad)} statement(s) rejected, first: {str(bad[0])[:250]}")
        empty = [n for n, b in enumerate(blocks) if not b.get("result")]
        if empty:
            sys.exit(
                f"{len(empty)} UPSERT(s) returned NO ROW despite status OK — first "
                f"{chunk[empty[0]][0]!r}. Refusing to report these as writes."
            )
        done += len(chunk)
    print(f"wrote {done} GB-COH entity event(s)")
    print("These are CANDIDATES ONLY — no link and no edge was created here.")
    print("Next: `enrich --source planning_identity`, then `project` and a bake.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
