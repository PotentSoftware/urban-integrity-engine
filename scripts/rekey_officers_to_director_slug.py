#!/usr/bin/env python3
"""Re-key officer nodes from per-company `OFF:{company}-{NAME}` to `director:{slug}`.

WHY (planning open item 17). `scripts/ch_backfill.py` wrote one officer node PER COMPANY,
so a person directing five companies became five unconnected nodes. Phoenix's motif is
seed Entity -> Officer -> Dissolved entity, which requires ONE officer node shared by two
companies; per-company keying makes that impossible, which is why 0 of 1,898 dissolved
companies had a usable edge. The load-bearing keying rule keys directors as
`director:{slug}` on the normalised name precisely to keep one human as one node.

SCOPE. This fixes Phoenix and nothing else. It yields no gate-3 benefit — 0 re-keyed
officer slugs match any planning applicant slug (Open Item 18). Do not expect land-flip
candidates from it.

THE SLUG. Uses the SHIPPED `mapper::applicant_slug` via `target/debug/applicant-slug`,
never a Python re-implementation. Two near-identical slug functions exist in this
codebase — `naming::slugify` joins with '_', `applicant_slug` with '-' — and choosing the
wrong one silently produces a third disjoint keyspace, i.e. the exact bug being repaired.

SAFETY. Rewrites `graph_events`, which is the append-only source of truth, so it is a data
correction rather than a routine write. Dry-run by default. The restore point is the
`.surql` export under /mnt/archive. A `project` re-run is required afterwards for the
change to reach the native tables.

Usage:
  python3 scripts/rekey_officers_to_director_slug.py                 # dry run + collision report
  python3 scripts/rekey_officers_to_director_slug.py --apply
"""
import sys, os, json, argparse, subprocess, collections

sys.path.insert(0, 'scripts')
import analyst_probe as ap

SLUG_BIN = 'target/debug/applicant-slug'
SLUG_BUILD = 'cargo build -p urban-integrity-ingestor --bin applicant-slug'


def slugify_all(names):
    """Slug every name via the shipped Rust function, in one batch, preserving order."""
    if not os.path.exists(SLUG_BIN):
        sys.exit(f"{SLUG_BIN} not built — run:\n  {SLUG_BUILD}")
    clean = [(n or '').replace('\n', ' ').replace('\r', ' ') for n in names]
    r = subprocess.run([SLUG_BIN], input='\n'.join(clean) + '\n',
                       capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"{SLUG_BIN} failed: {r.stderr[:300]}")
    out = r.stdout.splitlines()
    if len(out) != len(clean):
        sys.exit(f"{SLUG_BIN} returned {len(out)} slugs for {len(clean)} names — alignment broken")
    return out


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
    p = argparse.ArgumentParser()
    p.add_argument('--apply', action='store_true', help='write changes (default: dry run)')
    args = p.parse_args()

    db = ap.SurrealClient()
    print(f"analysis db: {db.ns}/{db.db}   mode: {'APPLY' if args.apply else 'DRY RUN'}\n")

    nodes = page(db, "id, payload.entity_id AS eid, payload.weight_json AS w",
                 "WHERE kind='NodeAdded' AND string::starts_with(payload.entity_id,'OFF:')")
    edges = page(db, "id, payload.dst_id AS dst",
                 "WHERE kind='EdgeAdded' AND string::starts_with(payload.dst_id,'OFF:')")
    print(f"officer NodeAdded events : {len(nodes)}")
    print(f"Directs EdgeAdded events : {len(edges)}")
    if not nodes:
        print("nothing to do")
        return

    # old OFF: id -> new director: id, derived from the node's OWN name field
    names = [json.loads(n['w']).get('name', '') for n in nodes]
    slugs = slugify_all(names)
    mapping, blank = {}, 0
    for n, nm, sl in zip(nodes, names, slugs):
        if not sl:
            blank += 1
            continue
        mapping[n['eid']] = f"director:{sl}"

    fused = collections.defaultdict(set)
    for old, new in mapping.items():
        fused[new].add(old)
    multi = {k: v for k, v in fused.items() if len(v) > 1}

    print(f"\ndistinct director: nodes after re-key : {len(fused)}")
    print(f"names slugging to empty (skipped)     : {blank}")
    print(f"nodes fusing >=2 source officers      : {len(multi)}")

    # The merge risk, made visible rather than silent: no date of birth exists in these
    # events, so same-name officers cannot be distinguished. Report every fusion.
    report = 'data/rekey-officer-collisions.tsv'
    with open(report, 'w') as fh:
        fh.write("new_id\tfused_count\tsource_officer_ids\n")
        for new, olds in sorted(multi.items(), key=lambda kv: -len(kv[1])):
            fh.write(f"{new}\t{len(olds)}\t{','.join(sorted(olds))}\n")
    print(f"collision report written              : {report}")
    for new, olds in sorted(multi.items(), key=lambda kv: -len(kv[1]))[:3]:
        print(f"    {new} <- {len(olds)} officers")

    if not args.apply:
        print("\nDRY RUN — no writes. Re-run with --apply, then `project` to reach native tables.")
        return

    print("\napplying…")

    def run_batch(items, field):
        """UPDATE `field` on a batch of records, values bound (never interpolated), and
        EVERY statement's status checked — a bare send would let individual rejections pass
        silently, which is how this project has lost writes before."""
        sql, params = [], {}
        for i, (key, new) in enumerate(items):
            sql.append(f"UPDATE type::record('graph_events', $k{i}) SET payload.{field} = $v{i};")
            params[f"k{i}"], params[f"v{i}"] = key, new
        blocks = db.query_multi_bound(''.join(sql), params)
        bad = [b for b in blocks if b.get('status') != 'OK']
        if bad:
            raise RuntimeError(f"{len(bad)} statement(s) rejected, first: {str(bad[0])[:200]}")
        return len(items)

    def apply_all(rows, id_key, field):
        work = []
        for r in rows:
            new = mapping.get(r[id_key])
            if new:
                work.append((str(r['id']).split(':', 1)[1], new))
        done = 0
        for i in range(0, len(work), 50):
            done += run_batch(work[i:i + 50], field)
            if done % 5000 < 50:
                print(f"  {field}: {done}/{len(work)}", flush=True)
        return done

    n_ok = apply_all(nodes, 'eid', 'entity_id')
    e_ok = apply_all(edges, 'dst', 'dst_id')
    print(f"\nrewrote {n_ok} node events, {e_ok} edge events")
    print("NOW RUN: flock -n /tmp/urban-project.lock ./target/debug/urban-integrity-cli "
          "project --analysis <db>   (~78 min)")


if __name__ == '__main__':
    main()
