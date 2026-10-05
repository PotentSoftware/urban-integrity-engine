#!/usr/bin/env python3
"""Backfill `date_published` onto CF contracts already in the graph, from `release.date`.

WHY (planning open item 19). `cf/mapper.rs` read only `tender.datePublished`, a Contracts
Finder extension present on ~17% of releases, and ignored `release.date`, which OCDS makes
mandatory and which is present on 100%. That left 4,929 of 5,552 CF contracts undated, so
Phoenix's `|award_date - dissolved_dt| <= 180` window could not be evaluated for any of its
candidate motifs. The mapper is fixed, but forward-only: `graph_events` stores the MAPPED
weight, not the raw OCDS, so existing rows cannot be repaired by re-projecting.

WHY NOT A RE-INGEST. `serve`'s CF poller takes roughly the 700 most recent notices and then
advances a cursor, so it would never revisit these. Seeding it from an early date would walk
the national firehose forward — thousands of pages — and, because the poller ingests
everything it fetches, would turn an Anglesey-scoped analysis into a national one. This
script instead fetches the same window and updates ONLY ocids already present, adding
nothing.

WINDOW. The CF contracts here were ingested on 4 days (2026-07-21/23/28, 2026-08-07), so
their notices were published shortly before. The default window covers that span with slack.

WRITES. Updates `payload.weight_json` on the CF `NodeAdded` events — the append-only source
of truth — so the change survives a re-projection. Phoenix hydrates from `graph_events`, so
a `serve` bake picks this up WITHOUT a full re-project. Dry-run by default.

Usage:
  python3 scripts/cf_backfill_dates.py                       # dry run
  python3 scripts/cf_backfill_dates.py --apply
  python3 scripts/cf_backfill_dates.py --from 2026-07-01 --apply
"""
import sys, json, argparse, urllib.request, urllib.parse, time

sys.path.insert(0, 'scripts')
import analyst_probe as ap

CF_SEARCH = "https://www.contractsfinder.service.gov.uk/Published/Notices/OCDS/Search"
DEFAULT_FROM = "2026-07-01T00:00:00+00:00"
MAX_PAGES = 400  # hard stop; the window should need far fewer. Never an unbounded walk.


def fetch_window(published_from, max_pages):
    """Yield releases from published_from forward, following the API's own pagination."""
    url = f"{CF_SEARCH}?publishedFrom={urllib.parse.quote(published_from)}&limit=100"
    for page in range(max_pages):
        d = None
        for attempt in range(5):
            try:
                with urllib.request.urlopen(url, timeout=45) as r:
                    d = json.loads(r.read().decode())
                break
            except urllib.error.HTTPError as e:
                if e.code == 429:
                    # Public service, shared rate limit. Back off rather than hammer it;
                    # a truncated window silently under-fills dates, so retry properly.
                    wait = 20 * (attempt + 1)
                    print(f"  [page {page}] 429 — backing off {wait}s (attempt {attempt+1}/5)", flush=True)
                    time.sleep(wait)
                    continue
                print(f"  [page {page}] HTTP {e.code} — stopping", flush=True)
                return
            except Exception as e:
                print(f"  [page {page}] fetch failed: {str(e)[:120]} — stopping", flush=True)
                return
        if d is None:
            print(f"  [page {page}] still rate-limited after 5 attempts — stopping", flush=True)
            return
        rel = d.get('releases') or []
        if not rel:
            return
        for x in rel:
            yield x
        nxt = (d.get('links') or {}).get('next')
        if not nxt:
            return
        url = nxt
        if page and page % 20 == 0:
            print(f"  …page {page}", flush=True)
        time.sleep(0.6)   # be polite to a public service; 429s appeared at ~0.15s
    print(f"  MAX_PAGES ({max_pages}) reached — window truncated, rerun with a later --from")


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--apply', action='store_true')
    p.add_argument('--from', dest='frm', default=DEFAULT_FROM)
    p.add_argument('--max-pages', type=int, default=MAX_PAGES)
    args = p.parse_args()

    db = ap.SurrealClient()
    print(f"analysis: {db.ns}/{db.db}   mode: {'APPLY' if args.apply else 'DRY RUN'}")

    # Existing CF contract events, indexed by ocid, with their current date state.
    rows, s = [], 0
    while True:
        r = db.query("SELECT id, payload.entity_id AS eid, payload.weight_json AS w "
                     f"FROM graph_events WHERE kind='NodeAdded' "
                     f"AND string::starts_with(payload.entity_id,'CF:') "
                     f"ORDER BY id LIMIT 20000 START {s};")
        if not r:
            break
        rows += r
        s += len(r)
    by_ocid, undated = {}, 0
    for r in rows:
        w = json.loads(r['w'])
        ocid = w.get('ocid') or r['eid'][3:]
        by_ocid.setdefault(ocid, []).append((r['id'], w))
        if not w.get('date_published'):
            undated += 1
    print(f"CF contract events: {len(rows)}   distinct ocids: {len(by_ocid)}   undated: {undated}\n")

    print(f"fetching CF releases published from {args.frm} …")
    seen = matched = filled = 0
    updates = []
    for rel in fetch_window(args.frm, args.max_pages):
        seen += 1
        ocid = rel.get('ocid')
        if ocid not in by_ocid:
            continue                      # NOT in our graph — deliberately ignored, never added
        matched += 1
        date = ((rel.get('tender') or {}).get('datePublished')) or rel.get('date')
        if not date:
            continue
        for rid, w in by_ocid[ocid]:
            if w.get('date_published'):
                continue                  # already dated — leave authoritative value alone
            w['date_published'] = date
            # Record ids containing hyphens come back BACKTICK-QUOTED
            # (graph_events:`NodeAdded_CF_ocds-...`). type::record() needs the bare key, so
            # strip them — leaving them in silently matches nothing, and SurrealDB reports
            # status OK for an UPDATE that affects zero rows.
            key = str(rid).split(':', 1)[1].strip('`')
            updates.append((key, json.dumps(w, separators=(',', ':'))))
            filled += 1

    print(f"\nreleases fetched : {seen}")
    print(f"  matched an ocid we hold : {matched}")
    print(f"  would fill a missing date: {filled}")
    if not args.apply:
        print("\nDRY RUN — no writes.")
        return
    if not updates:
        print("nothing to write")
        return

    print("\napplying…")
    done = 0
    for i in range(0, len(updates), 50):
        chunk = updates[i:i + 50]
        sql, params = [], {}
        for j, (key, wj) in enumerate(chunk):
            sql.append(f"UPDATE type::record('graph_events', $k{j}) SET payload.weight_json = $v{j};")
            params[f"k{j}"], params[f"v{j}"] = key, wj
        blocks = db.query_multi_bound(''.join(sql), params)
        bad = [b for b in blocks if b.get('status') != 'OK']
        if bad:
            raise SystemExit(f"{len(bad)} statement(s) rejected, first: {str(bad[0])[:200]}")
        # status OK is NOT sufficient: an UPDATE matching zero rows returns
        # {"status":"OK","result":[]}. Checking only the status reported 3,694 successful
        # writes for 3,694 no-ops on the first run of this script. Require a row back.
        empty = [i for i, b in enumerate(blocks) if not b.get('result')]
        if empty:
            raise SystemExit(
                f"{len(empty)} UPDATE(s) matched NO ROWS despite status OK — "
                f"first key: {chunk[empty[0]][0]!r}. Refusing to report these as writes.")
        done += len(chunk)
    print(f"wrote {done} event updates")
    print("Phoenix hydrates from graph_events — a `serve` bake picks this up with no re-project.")


if __name__ == '__main__':
    main()
