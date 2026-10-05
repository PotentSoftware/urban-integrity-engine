import sys, json, argparse, collections
sys.path.insert(0, 'scripts')
import analyst_probe as ap

PLACEHOLDER = "director:unknown-applicant"

db = ap.SurrealClient()


def page(fields, table, where=""):
    out, start = [], 0
    while True:
        rows = db.query(f"SELECT id, {fields} FROM {table} {where} ORDER BY id LIMIT 20000 START {start};")
        if not rows: break
        out.extend(rows); start += len(rows)
    return out


def s(v):
    if isinstance(v, str): return v
    if isinstance(v, dict):
        tb, i = v.get('tb'), v.get('id')
        if isinstance(i, dict): i = i.get('String') or i.get('Number')
        return f"{tb}:{i}" if tb else None
    return None


def compute_before_pairs():
    """PRE-fix shape: `located_at` keyed by applicant (`in.entity_id AS applicant`).

    This is the shape the live graph carries until 29-06's re-ingest replays
    29-03's contract-sourced mapper fix, so it is the CORRECT read for a
    BEFORE snapshot. For every applicant with exactly one contract and one
    location (excluding the unknown-applicant placeholder), emit the concrete
    (contract_id, location_id) pair, not just the count.
    """
    aw = page("in.entity_id AS applicant, out.entity_id AS contract", "awarded_to")
    lo = page("in.entity_id AS applicant, out.entity_id AS location", "located_at")
    apps = collections.defaultdict(set)
    locs = collections.defaultdict(set)
    for r in aw:
        a, c = s(r['applicant']), s(r['contract'])
        if a and c: apps[a].add(c)
    for r in lo:
        a, l = s(r['applicant']), s(r['location'])
        if a and l: locs[a].add(l)

    unamb_apps = unamb_applicants = 0
    ambiguous_apps = placeholder_apps = 0
    pairs = {}
    for a, cs in apps.items():
        if a == PLACEHOLDER:
            placeholder_apps += len(cs); continue
        ls = locs.get(a) or set()
        if len(cs) == 1 and len(ls) == 1:
            unamb_applicants += 1; unamb_apps += 1
            contract_id = next(iter(cs))
            location_id = next(iter(ls))
            pairs[contract_id] = location_id
        else:
            ambiguous_apps += len(cs)
    total = sum(len(c) for c in apps.values())
    stats = {
        "total": total,
        "unamb_apps": unamb_apps,
        "unamb_applicants": unamb_applicants,
        "ambiguous_apps": ambiguous_apps,
        "placeholder_apps": placeholder_apps,
    }
    return pairs, stats


def compute_current_located_at_by_contract():
    """POST-fix shape: `located_at` keyed directly by contract
    (`in.entity_id AS contract`) — no `awarded_to` chaining needed once the
    relation is contract-sourced (29-03/29-04)."""
    lo = page("in.entity_id AS contract, out.entity_id AS location", "located_at")
    locs = collections.defaultdict(set)
    for r in lo:
        c, l = s(r['contract']), s(r['location'])
        if c and l: locs[c].add(l)
    return locs


def cmd_snapshot(path):
    pairs, stats = compute_before_pairs()
    pct = (stats['unamb_apps'] * 100 // stats['total']) if stats['total'] else 0
    print(f"planning applications with an applicant : {stats['total']}")
    print(f"  unambiguously pairable NOW (1 app, 1 site): {stats['unamb_apps']}  ({pct}%)")
    print(f"     across {stats['unamb_applicants']} single-application applicants")
    print(f"  ambiguous (applicant has >1 app or >1 site): {stats['ambiguous_apps']}")
    print(f"  on the unknown-applicant placeholder       : {stats['placeholder_apps']}")
    with open(path, "w") as f:
        json.dump(pairs, f, indent=2, sort_keys=True)
    print(f"\nsnapshot written: {path}  ({len(pairs)} unambiguous (contract_id -> location_id) pairs)")


def cmd_diff(path):
    with open(path) as f:
        before = json.load(f)
    current = compute_current_located_at_by_contract()

    unchanged = missing = changed = 0
    missing_examples, changed_examples = [], []
    for contract_id, location_id in before.items():
        cur = current.get(contract_id)
        if not cur:
            missing += 1
            if len(missing_examples) < 10: missing_examples.append(contract_id)
        elif location_id in cur:
            unchanged += 1
        else:
            changed += 1
            if len(changed_examples) < 10:
                changed_examples.append((contract_id, location_id, sorted(cur)))

    print(f"before-snapshot pairs: {len(before)}")
    print(f"  unchanged (same contract -> same location): {unchanged}")
    print(f"  missing   (contract has no located_at now) : {missing}")
    print(f"  changed   (contract now resolves elsewhere): {changed}")
    if missing_examples:
        print(f"  missing examples: {missing_examples}")
    if changed_examples:
        print(f"  changed examples: {changed_examples}")

    if missing or changed:
        print("\nFAIL: non-zero missing/changed count — phase-blocking per D-06.")
        sys.exit(1)
    print("\nPASS: 100% of before-snapshot pairs present and unchanged after re-ingest.")


def main():
    parser = argparse.ArgumentParser(
        description="Land-flip application->site pairing validation harness (D-06)."
    )
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--snapshot", metavar="PATH", help="capture the BEFORE snapshot to PATH")
    group.add_argument("--diff", metavar="PATH", help="diff the CURRENT state against the snapshot at PATH")
    args = parser.parse_args()
    if args.snapshot:
        cmd_snapshot(args.snapshot)
    else:
        cmd_diff(args.diff)


if __name__ == "__main__":
    main()
