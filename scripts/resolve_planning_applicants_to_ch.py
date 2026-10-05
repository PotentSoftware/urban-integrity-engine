#!/usr/bin/env python3
"""Give planning applicants a real identifier by resolving them against Companies House.

WHY. Planning applicants enter the graph as free text — `director:{slug}` keyed on a name
and nothing else. Two consequences, measured live on 2026-08-11:

  * FALSE CLUSTERS. 47.5% of the 1,431 `AddressCluster` alerts collapse to fewer than three
    real parties once name variants are merged: `mr-a-mrs-j-example` and
    `mr-and-mrs-j-example` at one address is one couple counted twice. `l-d-c-ltd`,
    `y-d-c-limited`, `ydc-ltd` and `york-design-construction-ltd` are one firm counted four
    times.
  * BLIND SPOTS. ClwydAlyn Housing holds 52 Anglesey applications across 12 spellings; no
    node holds more than 32, so no motif can see the developer whole (Open Item 24).

Both are the same defect: the graph counts spellings, not parties.

WHAT ALREADY EXISTS, AND WHY IT WASN'T ENOUGH. `enricher/adapters/planning_identity.rs`
(IDN-05) already performs exactly this join — exact `normalise_name` equality, ambiguous
collisions resolve to NO match, and it writes both an `identity_links` audit row and a real
`Directs` graph edge. Its logic is sound. It matched **11 of 1,050** applicants carrying a
hard company suffix, because it can only match Companies House entities ALREADY IN THE
GRAPH — and the graph holds ~14,500 of the UK's ~5.4M companies. The candidate pool was
empty, not the matcher broken.

WHAT THIS DOES. Supplies the missing candidates. For every applicant whose name asserts it
is a registered company (a trailing `ltd`/`limited`/`cyf`/`plc`/`llp`), it asks Companies
House for that name and, on an unambiguous exact match, writes the real company into
`graph_events` as a `GB-COH:` entity. The reviewed adapter then makes the join itself, with
its own ambiguity handling and confidence tiers. This script deliberately creates NO link
and NO edge.

EXACT-MATCH-ONLY (D-18). CH's search endpoint is fuzzy; this script is not. Every candidate
CH returns is re-tested with `normalise_name(applicant) == normalise_name(company)` using
the SHIPPED function via `target/debug/normalise-name` — never a Python re-implementation,
because the adapter that consumes these candidates uses the Rust one and a drifted copy
would propose joins it then silently rejects. More than one exact match means AMBIGUOUS and
is dropped, mirroring `planning_identity.rs` and `backfill/ch.rs`. No score, no threshold,
no nearest match.

WHY A SUFFIX FILTER. "Gordon Owen Ltd" asserts corporate existence; "Mr G Owen" does not,
and searching it would invite exactly the false identification `T-23-FalseAccusation` names
as the gravest failure mode. Individuals are never searched.

Usage:
  python3 scripts/resolve_planning_applicants_to_ch.py              # dry run
  python3 scripts/resolve_planning_applicants_to_ch.py --apply
  python3 scripts/resolve_planning_applicants_to_ch.py --limit 50   # sample first
"""
import os, sys, json, time, base64, argparse, re, subprocess, urllib.request, urllib.error, collections

sys.path.insert(0, 'scripts')
import analyst_probe as ap

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = os.path.join(ROOT, "data", "ch-name-search-cache.jsonl")
OUT = os.path.join(ROOT, "data", "planning-applicant-ch-matches.tsv")
NORM_BIN = os.path.join(ROOT, "target", "debug", "normalise-name")
API = "https://api.company-information.service.gov.uk"
DELAY = 0.55       # CH allows 600 requests / 5 min = 2/s.

# A trailing company suffix is an ASSERTION of corporate existence. Anything without one is
# not searched — see the module docstring.
SUFFIX = re.compile(r"(^|[\s.,-])(ltd|limited|cyf|cyfyngedig|plc|llp)\.?$", re.I)

# UK postcode; group 1 is the AREA (the leading letters), the unit this corroborates on.
POSTCODE = re.compile(r"\b([A-Z]{1,2})(\d{1,2}[A-Z]?)\s*(\d[A-Z]{2})\b")


_SUFFIX_TOKENS = {"limited", "ltd", "plc", "llp", "llc", "cyf", "cyfyngedig"}


def strict_key(name):
    """A SECOND, stricter key that preserves `&`/`and`, used only to REJECT matches.

    `normalise_name` maps every non-alphanumeric character to a space, so `&` vanishes and
    `A K Developments Ltd` collapses onto `A & K DEVELOPMENTS LIMITED` — two different
    company names, one key. That was a live near-miss in the first sample run of this
    script, and auto-linking it would be `T-23-FalseAccusation` from a punctuation rule.

    This is a filter, never a matcher: a candidate must pass the shipped `normalise_name`
    equality FIRST, and this only removes some of what survives. It therefore cannot create
    a keyspace that diverges from the adapter's — it can only make this script propose
    less than the adapter would accept, which is the safe direction.
    """
    s = (name or "").lower().replace("&", " and ")
    s = "".join(c if (c.isalnum() or c.isspace()) else " " for c in s)
    toks = s.split()
    while toks and toks[-1] in _SUFFIX_TOKENS:
        toks.pop()
    return " ".join(toks)


def normalise_all(names):
    """Normalise via the SHIPPED Rust function, in one batch, preserving order."""
    if not os.path.exists(NORM_BIN):
        sys.exit(f"{NORM_BIN} not built — run:\n"
                 f"  cargo build -p urban-integrity-core --bin normalise-name")
    clean = [(n or "").replace("\n", " ").replace("\r", " ") for n in names]
    r = subprocess.run([NORM_BIN], input="\n".join(clean) + "\n", capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"{NORM_BIN} failed: {r.stderr[:300]}")
    out = r.stdout.splitlines()
    if len(out) != len(clean):
        sys.exit(f"{NORM_BIN} returned {len(out)} keys for {len(clean)} names — alignment broken")
    return out


class Ch:
    def __init__(self):
        key = os.environ.get("URBAN__ENRICHER__CH__API_KEY")
        if not key:
            raise SystemExit("URBAN__ENRICHER__CH__API_KEY not set (REST key, not streaming)")
        self.auth = "Basic " + base64.b64encode((key + ":").encode()).decode()

    def get(self, path):
        """GET an arbitrary CH REST path, honouring the rate limit and 429 backoff.

        Returns the decoded object, or None. **None means "no answer", not "no data"** —
        a 404 and a six-times-failed request are indistinguishable to the caller, so
        anything deciding on this must treat None as UNKNOWN and never as a negative
        finding. Corroboration that silently reads a transport failure as "no local
        officer" would withhold matches for the wrong reason.
        """
        req = urllib.request.Request(API + path, headers={"Authorization": self.auth})
        for attempt in range(6):
            try:
                with urllib.request.urlopen(req, timeout=45) as r:
                    body = json.loads(r.read().decode())
                    time.sleep(DELAY)
                    return body
            except urllib.error.HTTPError as e:
                if e.code == 404:
                    return None
                if e.code == 429:
                    wait = 30 * (attempt + 1)
                    print(f"    429 — backing off {wait}s", flush=True)
                    time.sleep(wait)
                    continue
                return None
            except Exception:
                time.sleep(5 * (attempt + 1))
        return None

    def search(self, name):
        q = urllib.parse.quote(name)
        req = urllib.request.Request(
            f"{API}/search/companies?q={q}&items_per_page=30",
            headers={"Authorization": self.auth})
        for attempt in range(6):
            try:
                with urllib.request.urlopen(req, timeout=45) as r:
                    time.sleep(DELAY)
                    return json.loads(r.read().decode()).get("items") or []
            except urllib.error.HTTPError as e:
                if e.code == 404:
                    return []
                if e.code == 429:
                    wait = 30 * (attempt + 1)
                    print(f"    429 — backing off {wait}s", flush=True)
                    time.sleep(wait)
                    continue
                return []
            except Exception:
                time.sleep(5 * (attempt + 1))
        return []


def load_cache():
    cache = {}
    if os.path.exists(CACHE):
        with open(CACHE) as f:
            for line in f:
                try:
                    r = json.loads(line)
                    cache[r["q"]] = r["items"]
                except Exception:
                    continue
    return cache


def page(db, sel, where):
    out, start = [], 0
    while True:
        rows = db.query(f"SELECT id, {sel} FROM graph_events {where} ORDER BY id LIMIT 20000 START {start};")
        if not rows:
            break
        out.extend(rows)
        start += len(rows)
    return out


def main():
    ap_ = argparse.ArgumentParser()
    ap_.add_argument("--apply", action="store_true")
    ap_.add_argument("--limit", type=int, default=0, help="only process the first N (sampling)")
    ap_.add_argument("--no-locality", action="store_true",
                     help="write matches with NO locality corroboration too (unsafe — see docstring)")
    args = ap_.parse_args()

    db = ap.SurrealClient()
    print(f"analysis: {db.ns}/{db.db}   mode: {'APPLY' if args.apply else 'DRY RUN'}")

    # Applicant identities and their original (unslugged) names.
    applicants = {}
    for r in page(db, "payload.entity_id AS eid, payload.weight_json AS w",
                  "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id ?? '', 'director:')"):
        eid = r["eid"]
        if not eid or eid in applicants:
            continue
        try:
            w = json.loads(r["w"] or "{}")
        except Exception:
            continue
        name = (w.get("name") or "").strip()
        if name:
            applicants[eid] = name

    # Companies already in the graph — matching those needs no API call and no new node.
    have = set()
    for r in page(db, "payload.entity_id AS eid",
                  "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id ?? '', 'GB-COH:')"):
        if r["eid"]:
            have.add(r["eid"])

    # The analysis's operating region, DERIVED from the wards it was actually given rather
    # than hardcoded. Every location this analysis holds contributes its postcode area; the
    # set grows by itself as more wards are configured, so this stays correct when the
    # system goes UK-wide. It is corroboration, never proof — it only decides whether a name
    # match is safe to auto-link or must go to a human.
    areas = set()
    for r in page(db, "payload.weight_json AS w",
                  "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id ?? '', 'location:')"):
        try:
            nm = (json.loads(r["w"] or "{}").get("name") or "").upper()
        except Exception:
            continue
        m = POSTCODE.search(nm)
        if m:
            areas.add(m.group(1))
    print(f"operating postcode areas implied by the configured wards: "
          f"{sorted(areas) if areas else '(none found)'}")

    targets = {e: n for e, n in applicants.items() if SUFFIX.search(n)}
    print(f"applicant identities: {len(applicants)}   assert corporate form: {len(targets)}   "
          f"GB-COH already in graph: {len(have)}")

    items = sorted(targets.items())
    if args.limit:
        items = items[:args.limit]

    cache = load_cache()
    ch = None
    keys = dict(zip([n for _e, n in items], normalise_all([n for _e, n in items])))

    rows, tally = [], collections.Counter()
    new_entities = {}
    fh = open(CACHE, "a")
    try:
        for i, (eid, name) in enumerate(items):
            want = keys[name]
            if not want:
                tally["applicant name normalises to empty"] += 1
                continue
            if name in cache:
                found = cache[name]
            else:
                if ch is None:
                    ch = Ch()
                found = ch.search(name)
                fh.write(json.dumps({"q": name, "items": found}) + "\n")
                fh.flush()
            if i and i % 100 == 0:
                print(f"  [{i}/{len(items)}] …", flush=True)

            titles = [(c.get("title") or "") for c in found]
            cand_keys = normalise_all(titles) if titles else []
            exact = [(c, k) for c, k in zip(found, cand_keys) if k == want]
            # Second gate: `normalise_name` elides `&`, so drop anything whose ampersand
            # structure differs from the applicant's. Only ever removes candidates.
            want_strict = strict_key(name)
            dropped_amp = [c for c, _k in exact if strict_key(c.get("title") or "") != want_strict]
            exact = [(c, k) for c, k in exact if strict_key(c.get("title") or "") == want_strict]
            if dropped_amp and not exact:
                tally["rejected — ampersand structure differs"] += 1
                rows.append((eid, name, ";".join(sorted(
                    x.get("company_number") or "" for x in dropped_amp)), "", "ampersand_mismatch"))
                continue

            if not exact:
                tally["no exact match at CH"] += 1
                rows.append((eid, name, "", "", "no_exact_match"))
                continue
            # Distinct company numbers matter, not distinct rows: CH can return the same
            # company more than once across name histories.
            by_num = {}
            for c, _k in exact:
                by_num.setdefault(c.get("company_number"), c)
            if len(by_num) > 1:
                # AMBIGUOUS. Mirrors planning_identity.rs / backfill/ch.rs — never an
                # arbitrary pick, because the wrong pick is a false accusation.
                tally["AMBIGUOUS — several companies share the name"] += 1
                rows.append((eid, name, ";".join(sorted(by_num)), "", "ambiguous"))
                continue

            num, c = next(iter(by_num.items()))
            gid = f"GB-COH:{num}"
            # LOCALITY CORROBORATION. An exact name match yields a UNIQUE company (CH names
            # are unique) but not necessarily the RIGHT one: live, the Anglesey applicant
            # "LDC Ltd" on a Menai Bridge boatyard matched LDC LIMITED of Guildford, 250
            # miles away, while the local firm was a different company entirely. A short
            # generic name will match a same-named company anywhere in Britain.
            #
            # A registered office outside the analysis's own operating areas is NOT rejected
            # — national firms (Stena Line Ports, Premier Inn) legitimately build here from
            # distant head offices. It is withheld from auto-linking and reported for a
            # human instead, because `planning_identity.rs` auto-links a full-name match
            # with no review and writes a real graph edge.
            snip = (c.get("address_snippet") or "").upper()
            pm = POSTCODE.search(snip)
            local = bool(areas) and bool(pm) and pm.group(1) in areas
            if not local and not args.no_locality:
                tally["match withheld — office outside the analysis's areas (needs review)"] += 1
                rows.append((eid, name, num, c.get("title", ""), "review_offsite"))
                continue
            if gid in have:
                tally["matched a company ALREADY in the graph"] += 1
                rows.append((eid, name, num, c.get("title", ""), "already_present"))
                continue
            tally["MATCHED — new company to add"] += 1
            rows.append((eid, name, num, c.get("title", ""), "new"))
            new_entities[gid] = c
    finally:
        fh.close()

    with open(OUT, "w") as f:
        f.write("applicant_id\tapplicant_name\tcompany_number\tcompany_name\toutcome\n")
        for r in rows:
            f.write("\t".join(str(x) for x in r) + "\n")

    print(f"\nprocessed {len(items)} applicant(s):")
    for k, v in tally.most_common():
        print(f"  {v:>6}  {k}")
    print(f"\nwritten: {OUT}")
    print(f"companies to add to the graph: {len(new_entities)}")

    if not args.apply:
        print("\nDRY RUN — no writes. Rerun with --apply.")
        return
    if not new_entities:
        print("nothing to write")
        return

    print("\nwriting GB-COH entities to graph_events…")
    ts = int(time.time() * 1000)
    done = 0
    keyed = sorted(new_entities.items())
    for i in range(0, len(keyed), 50):
        chunk = keyed[i:i + 50]
        sql, params = [], {}
        for j, (gid, c) in enumerate(chunk):
            status = "Dissolved" if c.get("company_status") == "dissolved" else "Active"
            st = {"type": status}
            if status == "Dissolved":
                d = (c.get("date_of_cessation") or "")[:10]
                # Phoenix cannot window an undated cessation; epoch is the mapper's own
                # sentinel for "dissolved, date unknown".
                st["content"] = f"{d}T00:00:00Z" if d else "1970-01-01T00:00:00Z"
            w = {"type": "Entity", "name": c.get("title") or "",
                 "company_number": gid.split(":", 1)[1], "status": st}
            if (c.get("date_of_creation") or ""):
                w["incorporation_date"] = c["date_of_creation"][:10]
            sql.append(
                f"UPSERT type::record('graph_events', $k{j}) CONTENT {{"
                f'"event_id": $e{j}, "timestamp": {ts}, "kind": "NodeAdded", '
                f'"payload": {{"entity_id": $i{j}, "weight_json": $v{j}}}}};')
            params[f"k{j}"] = f"NodeAdded_applicant_resolve_{gid.split(':', 1)[1]}"
            params[f"e{j}"] = f"applicant-resolve-{gid.split(':', 1)[1]}"
            params[f"i{j}"] = gid
            params[f"v{j}"] = json.dumps(w, separators=(",", ":"))
        blocks = db.query_multi_bound("".join(sql), params)
        bad = [b for b in blocks if b.get("status") != "OK"]
        if bad:
            raise SystemExit(f"{len(bad)} statement(s) rejected, first: {str(bad[0])[:250]}")
        empty = [n for n, b in enumerate(blocks) if not b.get("result")]
        if empty:
            raise SystemExit(
                f"{len(empty)} UPSERT(s) returned NO ROW despite status OK — first "
                f"{chunk[empty[0]][0]!r}. Refusing to report these as writes.")
        done += len(chunk)
    print(f"wrote {done} GB-COH entity event(s)")
    print("These are CANDIDATES ONLY — no link and no edge was created here.")
    print("Next: `enrich --source planning_identity` makes the join, with its own exact-name")
    print("      and ambiguity rules, then `project` and a bake.")


if __name__ == "__main__":
    main()
