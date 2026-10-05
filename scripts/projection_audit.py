#!/usr/bin/env python3
"""
projection_audit.py — is each projected table TRUNCATED, STALE, DROPPED, or COMPLETE?

`graph_events` is the source of truth; the projected tables are a derived view that
`project` rebuilds. Open Item 30 recorded that the view holds a fraction of the graph.
"A fraction" has two very different causes and two different fixes:

  TRUNCATED  the missing rows sit BEYOND the point the last `project` run reached.
             The projector never attempted them. Fix: re-run `project`.
  DROPPED    rows the run demonstrably walked past are absent anyway.
             Fix: find out why the projector dropped them — a re-run will not help.

**Compare in the projector's own load order, not by timestamp.** `Projector::run`
pages the log `ORDER BY id` (crates/ingestor/src/projector.rs:477), and `graph_events`
carries two id families — SurrealDB-random base36 ids from the CH/planning streams and
deterministic `EdgeAdded_*` strings from the CF poller and the enricher. Lexically the
random ids sort into blocks either side of the deterministic ones, so id order and
timestamp order are ~40% inverted. The first version of this script split STALE from
LOSSY on `timestamp` and mislabelled every table of an interrupted run as LOSSY: the
2026-08-11 run was SIGTERMed 62,777 rows into a 498,986-row edge stream, and because
its id-order prefix is a scattered *time* sample spanning the whole corpus, five tables
looked like "nothing projected at all" and four like "rows dropped inside the window".
All nine were one truncated run. See the projection-truncation investigation notes.

This distinguishes them per table in one pass over the log, because the alternative is
one full scan per table and the log is ~500k rows.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import json

import surreal_target

NODE_TABLES = {
    "Entity": "entity", "Officer": "officer", "Location": "location",
    "Contract": "contract", "Ward": "ward", "PoliticalActor": "political_actor",
    "Psc": "psc", "Charity": "charity", "Property": "property",
    "OffshoreEntity": "offshore_entity",
}
EDGE_TABLES = {
    "Directs": "directs", "AwardedTo": "awarded_to", "LocatedAt": "located_at",
    "Controls": "controls", "IssuedBy": "issued_by", "Represents": "represents",
    "TrusteeOf": "trustee_of", "DonatesTo": "donates_to", "OwnsProperty": "owns_property",
    "DisqualifiedFrom": "disqualified_from", "OffshoreLink": "offshore_link",
}

TARGET = None


def q(sql, timeout=1800):
    out = TARGET.query(sql, timeout=timeout)
    for r in out:
        if r.get("status") != "OK":
            raise SystemExit(f"query failed: {r}")
    return [r["result"] for r in out]


def ts(ms):
    return dt.datetime.fromtimestamp(ms / 1000, dt.timezone.utc).strftime("%Y-%m-%d %H:%M")


def strip(rid: str) -> str:
    """`table:⟨id⟩` -> `id`. `<string>` escapes backslashes; undo that or the set
    comparison invents phantom missing rows (16 of them on `located_at`)."""
    inner = rid.split(":", 1)[1] if ":" in rid else rid
    return inner.strip("`").replace("\\\\", "\\")


def main():
    global TARGET
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    surreal_target.add_analysis_arg(p)
    args = p.parse_args()
    TARGET = surreal_target.resolve(args)
    TARGET.preflight()
    print(TARGET.banner())

    run_marker()

    print("scanning graph_events (one pass, in the projector's load order) ...", flush=True)
    nodes, = q("SELECT id, payload.entity_id AS eid, payload.weight_json AS w, timestamp "
               "FROM graph_events WHERE kind IN ['NodeAdded','NodeUpdated'] ORDER BY id;")
    edges, = q("SELECT id, payload.src_id AS s, payload.dst_id AS d, payload.weight_json AS w, timestamp "
               "FROM graph_events WHERE kind='EdgeAdded' ORDER BY id;")
    print(f"  node events {len(nodes):,}   edge events {len(edges):,}", flush=True)

    # `ORDER BY id` above is load order, so a row's position in these lists is exactly
    # the position at which the projector would reach it. Keep the FIRST position per
    # key: that is the earliest point a completed run could have written it.
    # Both coordinates are needed. Load index alone cannot classify a pass that ran to
    # completion, because its cut sits at the end of TODAY's log and events appended since
    # the run sort below it. Timestamp alone cannot classify a truncated pass, because the
    # id-order prefix is a scattered time sample. A row is only genuinely dropped if the run
    # both reached its position AND already existed when the run read the log.
    log_nodes: dict[str, dict] = collections.defaultdict(dict)   # type -> id -> (index, ts)
    for i, r in enumerate(nodes):
        eid, w = r.get("eid"), r.get("w")
        if not eid or not w:
            continue
        t = json.loads(w).get("type")
        if t in NODE_TABLES:
            keep(log_nodes[t], eid, i, r["timestamp"])

    log_edges: dict[str, dict] = collections.defaultdict(dict)   # type -> (s,d) -> (index, ts)
    for i, r in enumerate(edges):
        s_, d_, w = r.get("s"), r.get("d"), r.get("w")
        if not s_ or not d_ or not w:
            continue
        t = json.loads(w).get("type")
        if t in EDGE_TABLES:
            keep(log_edges[t], (s_, d_), i, r["timestamp"])

    # Collect the projected sets first: the cut position is a property of the RUN, not of
    # any one table, so it can only be computed once every projected row is known.
    got_nodes, got_edges = {}, {}
    for kind, table in sorted(NODE_TABLES.items()):
        if not log_nodes.get(kind):
            continue
        got_r, = q(f"SELECT entity_id FROM {table};")
        got_nodes[kind] = {x["entity_id"] for x in got_r if x.get("entity_id")}
    for kind, table in sorted(EDGE_TABLES.items()):
        if not log_edges.get(kind):
            continue
        # `record::id()`, NOT `<string>`. The string cast renders the id with SurrealQL
        # escaping applied — a CR becomes the two characters `\r`, a backtick becomes
        # `` \` `` — so comparing it against the raw id in the log invents missing rows for
        # every id containing a control character or a quote. That artefact is what made
        # 12 correctly-projected `located_at` edges look like write loss, and sent a
        # "backslash round-trip" bug (L6) into the finding doc that does not exist:
        # `record::exists()` reports zero dangling endpoints across all 11 relation tables.
        got_r, = q(f"SELECT record::id(in) AS a, record::id(out) AS b FROM {table};")
        got_edges[kind] = {(x["a"], x["b"]) for x in got_r if x.get("a") and x.get("b")}

    node_cut, node_ts = high_water(log_nodes, got_nodes)
    edge_cut, edge_ts = high_water(log_edges, got_edges)

    # An edge whose endpoint has no node event anywhere in the log can never be projected:
    # there is nothing to RELATE to. Those are dangling source data, not projector loss,
    # and the projector already counts them as skipped_unknown_src/dst.
    all_node_ids = {eid for keys in log_nodes.values() for eid in keys}

    rows = []
    for kind, table in sorted(NODE_TABLES.items()):
        if kind in got_nodes:
            rows.append(classify(table, log_nodes[kind], got_nodes[kind], node_cut, node_ts))
    for kind, table in sorted(EDGE_TABLES.items()):
        if kind in got_edges:
            rows.append(classify(table, log_edges[kind], got_edges[kind], edge_cut, edge_ts,
                                 all_node_ids))

    print(f"\n{'table':<20}{'in log':>9}{'projected':>11}{'missing':>9}  {'verdict':<10} detail")
    print("-" * 108)
    for r in rows:
        print(f"{r['table']:<20}{r['want']:>9,}{r['got']:>11,}{r['missing']:>9,}  "
              f"{r['verdict']:<10} {r['detail']}")

    for label, cut, total in (("node", node_cut, len(nodes)), ("edge", edge_cut, len(edges))):
        pct = 100.0 * (cut + 1) / total if total else 0.0
        state = "ran to completion" if pct > 99.9 else "STOPPED EARLY — re-run `project`"
        print(f"\n{label} pass reached position {cut + 1:,} of {total:,} ({pct:.1f}%) — {state}")

    dropped = [r for r in rows if r["verdict"] == "DROPPED"]
    if dropped:
        print(f"\n{len(dropped)} of {len(rows)} tables are missing rows the run walked past — "
              f"a re-run of `project` will NOT fix those:")
        for r in dropped:
            print(f"  {r['table']}: {r['inside']:,} missing below the cut")
    else:
        dangling = sum(r["missing"] for r in rows)
        if dangling:
            print(f"\nNo table is missing a row the run walked past. The {dangling:,} remaining "
                  f"gaps are edges whose endpoint has no node event anywhere in the log — "
                  f"nothing to RELATE to. Compare against the run's own "
                  f"skipped_unknown_src/dst: they should agree exactly.")
        else:
            print("\nEvery table matches the log exactly.")


def run_marker() -> None:
    """Report whether the last `project` run finished — the first thing that matters.

    Everything below this line compares the projection against the log, but a projection
    left behind by a killed run is not evidence about the projector at all. Written by
    `Projector::begin_run` / `finish_run`; absent on databases predating Open Item 30.
    """
    rows, = q("SELECT * FROM projection_runs:current;")
    if not rows:
        print("last project run: NO MARKER — predates the run marker, or `project` has "
              "never run here. Verdicts below cannot rule out a truncated run.")
        return
    m = rows[0]
    if m.get("finished_at_ms"):
        print(f"last project run: COMPLETE at {ts(m['finished_at_ms'])} — "
              f"{m.get('nodes_upserted', 0):,} nodes, {m.get('edges_related', 0):,} edges, "
              f"{m.get('edges_skipped', 0):,} skipped")
    else:
        print(f"last project run: ** NEVER FINISHED ** (started {ts(m['started_at_ms'])}). "
              f"The tables are a partial rebuild. Re-run `project` before reading anything "
              f"into the verdicts below.")


def keep(bucket: dict, key, idx: int, timestamp: int) -> None:
    """Track a key's EARLIEST load position and EARLIEST timestamp — which are different
    events, because id order and time order are ~40% inverted. The position is where a
    completed run would first have written the key; the timestamp is when a run first
    could have seen it, and is what dates the run itself.
    """
    cur = bucket.get(key)
    bucket[key] = (idx, timestamp) if cur is None else (min(cur[0], idx), min(cur[1], timestamp))


def high_water(log: dict, got: dict) -> tuple[int, int]:
    """How far the last `project` run got, in both coordinates.

    Returns (highest load-order position reached, newest event timestamp it wrote). The run
    consumes one ordered stream, so both are single numbers across all tables of a class: a
    table whose events all sort above the position was never attempted, which is why a
    per-table view of the same data reads as "nothing projected at all".
    """
    hit = [log[kind][key] for kind, keys in got.items() for key in keys if key in log[kind]]
    if not hit:
        return -1, -1
    return max(i for i, _ in hit), max(t for _, t in hit)

def classify(table, want: dict, got: set, cut: int, cut_ts: int, node_ids: set | None = None) -> dict:
    missing_keys = [k for k in want if k not in got]
    dangling = 0
    if node_ids is not None:
        dangling = sum(1 for k in missing_keys
                       if k[0] not in node_ids or k[1] not in node_ids)
        missing_keys = [k for k in missing_keys
                        if k[0] in node_ids and k[1] in node_ids]
    missing = [want[k] for k in missing_keys]
    base = {"table": table, "want": len(want), "got": len(got),
            "missing": len(missing) + dangling}
    dang = f" ({dangling:,} dangling: an endpoint has no node event)" if dangling else ""
    if not missing:
        return {**base, "verdict": "COMPLETE", "detail": dang.strip(), "inside": 0}
    unreached = sum(1 for idx, _ in missing if idx > cut)
    appended = sum(1 for idx, t in missing if idx <= cut and t > cut_ts)
    dropped = len(missing) - unreached - appended
    if dropped:
        return {**base, "verdict": "DROPPED", "inside": dropped,
                "detail": f"{dropped:,} the run reached and did not write "
                          f"({unreached:,} never reached, {appended:,} logged since){dang}"}
    if unreached:
        return {**base, "verdict": "TRUNCATED", "inside": 0,
                "detail": f"all missing rows sort past position {cut + 1:,} — never "
                          f"attempted{dang}"}
    return {**base, "verdict": "STALE", "inside": 0,
            "detail": f"all missing rows logged after {ts(cut_ts)} — the run predates "
                      f"them{dang}"}


if __name__ == "__main__":
    main()
