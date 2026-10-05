#!/usr/bin/env python3
"""Widen Phoenix's failed-company coverage: fetch real `date_of_cessation` from CH REST.

WHY (planning open item 6c, closed 2026-08-11). The dissolution dates in the graph are
CORRECT — 40/40 sampled match Companies House exactly. The defect that finding uncovered is
COVERAGE, not correctness. The CH company-profile stream only emits companies that CHANGE,
so the graph's entire dissolved corpus is one day wide: 1,965 entities, every one ceasing
2026-07-28, which was a national bulk-dissolution batch (14,974 companies, against 103 and
161 on the adjacent days). A 150-company control sample of entities the graph calls Active
found 7.5% are dissolved at CH (~942 extrapolated) — and 11 of 11 also ceased on that same
day, i.e. the stream simply missed part of one batch.

Phoenix windows on `|award_date - dissolved_dt| <= 180`, so a company the graph wrongly
believes is Active can never be the failed counterparty in a motif. Coverage is therefore a
hard ceiling on the flagship detector, and it is currently 13% of a single day.

WHAT THIS DOES. Phase 1 asks CH REST for the current profile of every GB-COH entity the
graph does NOT already record as Dissolved/InLiquidation, caching each reply. Phase 2
(`--apply`) writes a `NodeUpdated` event for each one CH reports as ceased, carrying the
REAL `date_of_cessation` rather than the `1970-01-01` placeholder `ch_backfill.py` emits.

FIDELITY. The new weight is the EXISTING weight with only `status` replaced — name,
company_number and incorporation_date are preserved. Dropping fields here would repeat the
projector defect that cost Phase 29 two false root causes.

IDEMPOTENT. Events are UPSERTed on a deterministic record id (`NodeUpdated_cessation_{cn}`),
so a rerun overwrites rather than accumulating. Every write is verified to return a row —
SurrealDB reports `status: OK` with an empty result for an UPDATE that matched nothing, and
trusting the status alone has already reported 3,694 phantom writes on this project once.

Usage:
  python3 scripts/ch_cessation_backfill.py --fetch          # phase 1 only (~2h, resumable)
  python3 scripts/ch_cessation_backfill.py                  # phase 2 dry run
  python3 scripts/ch_cessation_backfill.py --apply          # phase 2 write
"""
import os, sys, json, time, base64, argparse, urllib.request, urllib.error, collections

sys.path.insert(0, 'scripts')
import analyst_probe as ap

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = os.path.join(ROOT, "data", "ch-company-status-cache.jsonl")
API = "https://api.company-information.service.gov.uk"
DELAY = 0.55           # CH allows 600 requests / 5 min = 2/s. Stay under it.
CEASED = {"dissolved", "liquidation", "receivership", "administration"}


def auth_header():
    key = os.environ.get("URBAN__ENRICHER__CH__API_KEY")
    if not key:
        raise SystemExit("URBAN__ENRICHER__CH__API_KEY not set (REST key, not streaming)")
    return "Basic " + base64.b64encode((key + ":").encode()).decode()


def fetch_profile(cn, auth):
    req = urllib.request.Request(f"{API}/company/{cn}", headers={"Authorization": auth})
    for attempt in range(6):
        try:
            with urllib.request.urlopen(req, timeout=45) as r:
                time.sleep(DELAY)
                return json.loads(r.read().decode())
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return {"_gone": True}
            if e.code == 429:
                wait = 30 * (attempt + 1)
                print(f"    429 — backing off {wait}s", flush=True)
                time.sleep(wait)
                continue
            return {"_error": e.code}
        except Exception:
            time.sleep(5 * (attempt + 1))
    return {"_error": "retries-exhausted"}


def page_all(db):
    """Every GB-COH node event, with its current weight."""
    out, start = [], 0
    while True:
        rows = db.query(
            "SELECT id, payload.entity_id AS eid, payload.weight_json AS w FROM graph_events "
            "WHERE string::starts_with(payload.entity_id ?? '', 'GB-COH:') "
            f"ORDER BY id LIMIT 20000 START {start};")
        if not rows:
            break
        out.extend(rows)
        start += len(rows)
    return out


def current_state(db):
    """entity_id -> (company_number, latest weight dict, already_ceased)."""
    state = {}
    for r in page_all(db):
        try:
            w = json.loads(r["w"] or "{}")
        except Exception:
            continue
        eid = r["eid"]
        ceased = (w.get("status") or {}).get("type") in ("Dissolved", "InLiquidation")
        prev = state.get(eid)
        # A NodeUpdated can follow a NodeAdded; the ceased one is the later truth.
        if prev is None or ceased:
            state[eid] = (eid.split(":", 1)[1], w, ceased)
    return state


def load_cache():
    cache = {}
    if os.path.exists(CACHE):
        with open(CACHE) as f:
            for line in f:
                try:
                    r = json.loads(line)
                    cache[r["cn"]] = r["p"]
                except Exception:
                    continue
    return cache


def main():
    ap_ = argparse.ArgumentParser()
    ap_.add_argument("--fetch", action="store_true", help="phase 1 only")
    ap_.add_argument("--apply", action="store_true", help="phase 2 write")
    args = ap_.parse_args()

    db = ap.SurrealClient()
    state = current_state(db)
    active = sorted({cn for cn, _w, ceased in state.values() if not ceased})
    print(f"analysis: {db.ns}/{db.db}")
    print(f"GB-COH entities: {len(state)}   already ceased: {len(state)-len(active)}   "
          f"to check: {len(active)}")

    cache = load_cache()
    todo = [cn for cn in active if cn not in cache]
    print(f"cached: {len(active)-len(todo)}   to fetch: {len(todo)}")

    if todo:
        auth = auth_header()
        t0 = time.time()
        with open(CACHE, "a") as f:
            for i, cn in enumerate(todo):
                p = fetch_profile(cn, auth)
                keep = {k: p.get(k) for k in
                        ("company_status", "date_of_cessation", "company_name",
                         "date_of_creation", "_gone", "_error")}
                cache[cn] = keep
                f.write(json.dumps({"cn": cn, "p": keep}) + "\n")
                f.flush()
                if i and i % 250 == 0:
                    rate = i / max(time.time() - t0, 1)
                    print(f"  [{i}/{len(todo)}] {rate*60:.0f}/min, "
                          f"~{(len(todo)-i)/max(rate,0.01)/60:.0f} min left", flush=True)
        print(f"fetched {len(todo)}\n")

    if args.fetch:
        print("phase 1 complete — rerun without --fetch to see the write plan")
        return

    # Phase 2: plan the updates.
    st = collections.Counter()
    updates = []
    for eid, (cn, w, ceased) in sorted(state.items()):
        if ceased:
            continue
        p = cache.get(cn)
        if not p:
            st["not-fetched"] += 1
            continue
        s = p.get("company_status")
        if p.get("_gone"):
            st["404-at-ch"] += 1
            continue
        if p.get("_error") or not s:
            st["fetch-error"] += 1
            continue
        if s not in CEASED:
            st[f"still-{s}"] += 1
            continue
        d = p.get("date_of_cessation")
        if not d:
            # CH says ceased but published no date. Phoenix cannot window an undated
            # cessation, so writing one would add a node it must then discard.
            st["ceased-but-undated"] += 1
            continue
        kind = "Dissolved" if s == "dissolved" else "InLiquidation"
        neww = dict(w)                      # preserve name/company_number/incorporation_date
        neww["status"] = {"type": kind, "content": f"{d}T00:00:00Z"}
        updates.append((cn, eid, json.dumps(neww, separators=(",", ":")), d, kind))
        st[f"WILL-UPDATE-{kind}"] += 1

    print("plan:")
    for k, v in st.most_common():
        print(f"  {k:<28}{v:>7}")
    print(f"\nentities to mark ceased: {len(updates)}")
    if updates:
        months = collections.Counter(d[:7] for _c, _e, _w, d, _k in updates)
        print("cessation months:", dict(months.most_common(12)))

    if not args.apply:
        print("\nDRY RUN — no writes. Rerun with --apply.")
        return
    if not updates:
        print("nothing to write")
        return

    print("\napplying…")
    ts = int(time.time() * 1000)
    done = 0
    for i in range(0, len(updates), 50):
        chunk = updates[i:i + 50]
        sql, params = [], {}
        for j, (cn, eid, wj, _d, _k) in enumerate(chunk):
            sql.append(
                f"UPSERT type::record('graph_events', $k{j}) CONTENT {{"
                f'"event_id": $e{j}, "timestamp": {ts}, "kind": "NodeUpdated", '
                f'"payload": {{"entity_id": $i{j}, "weight_json": $v{j}}}}};')
            params[f"k{j}"] = f"NodeUpdated_cessation_{cn}"
            params[f"e{j}"] = f"ch-cessation-{cn}"
            params[f"i{j}"] = eid
            params[f"v{j}"] = wj
        blocks = db.query_multi_bound(''.join(sql), params)
        bad = [b for b in blocks if b.get('status') != 'OK']
        if bad:
            raise SystemExit(f"{len(bad)} statement(s) rejected, first: {str(bad[0])[:250]}")
        empty = [n for n, b in enumerate(blocks) if not b.get('result')]
        if empty:
            raise SystemExit(
                f"{len(empty)} UPSERT(s) returned NO ROW despite status OK — first cn "
                f"{chunk[empty[0]][0]!r}. Refusing to report these as writes.")
        done += len(chunk)
        if i % 1000 == 0:
            print(f"  {done}/{len(updates)}", flush=True)
    print(f"wrote {done} NodeUpdated event(s)")
    print("Run `project` for the native tables, then a `serve` bake to re-derive alerts.")


if __name__ == "__main__":
    main()
