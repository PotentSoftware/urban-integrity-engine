import sys, os, json, random, collections, argparse, subprocess
sys.path.insert(0, 'scripts')
import analyst_probe as ap
db = ap.SurrealClient()


def s(v):
    if isinstance(v, str):
        return v
    if isinstance(v, dict):
        tb, i = v.get('tb'), v.get('id')
        if isinstance(i, dict):
            i = i.get('String') or i.get('Number')
        return f"{tb}:{i}" if tb else None
    return None


# ──────────────────────────────────────────────────────────────────────────
# --fp-sample: stratified false-positive adjudication evidence file (29-08
# Task 2 / the phase 29 validation notes' "False-positive rate of emitted candidates"
# manual-only verification). Sample ALL pending-review candidates if the
# total is <=30, else N=30 stratified by tier proportional to each tier's
# share of the total — drawn BEFORE anyone looks at which candidates "look
# interesting" (T-29-08). A fixed seed makes the sample reproducible against
# the commit that produced it, per the phase 29 validation notes' own requirement.
# ──────────────────────────────────────────────────────────────────────────
FP_SAMPLE_SEED = 29080  # phase-plan mnemonic; fixed so the sample is reproducible
FP_SAMPLE_SIZE = 30


def _application_address(planning_ref: str):
    """Look up the original planning application's free-text address and
    postcode by joining contract(application_ref) -> located_at -> location.
    Returns (address, postcode); either may be None if the join fails (should
    not happen for a written candidate, which required this join to succeed
    at gate-1 time, but this script is read-only and defensive regardless).

    All three hops bind every interpolated value via SurrealClient.query_bound
    (SurrealDB's HTTP /sql query-string variable binding) rather than string
    interpolation. `planning_ref` traces back to a scraped, unvalidated
    planning-portal `reference` field, and `contract_id`/`location_id` are
    read back out of the DB from that same unvalidated origin — none of them
    are safe to hand-escape (CR-01, phase 29 code review)."""
    con = db.query_bound(
        "SELECT entity_id FROM contract WHERE application_ref = $ref LIMIT 1;",
        {"ref": planning_ref},
    )
    if not con:
        return None, None
    contract_id = s(con[0].get('entity_id'))
    if not contract_id:
        return None, None
    lo = db.query_bound(
        "SELECT out.entity_id AS location FROM located_at WHERE in.entity_id = $contract_id LIMIT 1;",
        {"contract_id": contract_id},
    )
    if not lo:
        return None, None
    location_id = s(lo[0].get('location'))
    if not location_id:
        return None, None
    locrows = db.query_bound(
        "SELECT postcode, name FROM location WHERE entity_id = $location_id LIMIT 1;",
        {"location_id": location_id},
    )
    if not locrows:
        return None, None
    return locrows[0].get('name'), locrows[0].get('postcode')


def fp_sample(out_path: str) -> None:
    candidates = db.query(
        "SELECT id, tier, severity, planning_ref, ppd_transaction_id, ppd_postcode, "
        "ppd_paon, ppd_street, ppd_price_gbp, ppd_date_of_transfer, temporal_offset_days, "
        "corroborating_link_id, corroborating_link_confidence, corroborating_entity_type "
        "FROM land_flip_candidates WHERE status = 'pending_review';"
    )
    total = len(candidates)
    print(f"pending_review candidates: {total}")

    if total <= FP_SAMPLE_SIZE:
        sampled = candidates
        print(f"total <= {FP_SAMPLE_SIZE} — sampling ALL {total} candidate(s)")
    else:
        by_tier = collections.defaultdict(list)
        for c in candidates:
            by_tier[c.get('tier') or 'UNKNOWN'].append(c)
        rng = random.Random(FP_SAMPLE_SEED)
        sampled = []
        for tier, rows in sorted(by_tier.items()):
            share = round(FP_SAMPLE_SIZE * len(rows) / total)
            share = min(share, len(rows))
            sampled.extend(rng.sample(rows, share))
        # Rounding can land a couple short/over FP_SAMPLE_SIZE; top up/trim
        # deterministically from the same rng rather than hand-picking.
        if len(sampled) < FP_SAMPLE_SIZE:
            remaining = [c for c in candidates if c not in sampled]
            rng.shuffle(remaining)
            sampled.extend(remaining[: FP_SAMPLE_SIZE - len(sampled)])
        elif len(sampled) > FP_SAMPLE_SIZE:
            rng.shuffle(sampled)
            sampled = sampled[:FP_SAMPLE_SIZE]
        print(
            f"total > {FP_SAMPLE_SIZE} — stratified sample of {len(sampled)} "
            f"by tier share: { {t: len(r) for t, r in sorted(by_tier.items())} }"
        )

    rows_out = []
    for c in sampled:
        planning_ref = c.get('planning_ref')
        address, app_postcode = _application_address(planning_ref) if planning_ref else (None, None)
        rows_out.append({
            "planning_ref": planning_ref,
            "application_address": address,
            "application_postcode": app_postcode,
            "ppd_transaction_id": c.get('ppd_transaction_id'),
            "ppd_postcode": c.get('ppd_postcode'),
            "ppd_paon": c.get('ppd_paon'),
            "ppd_street": c.get('ppd_street'),
            "ppd_price_gbp": c.get('ppd_price_gbp'),
            "ppd_date_of_transfer": c.get('ppd_date_of_transfer'),
            "tier": c.get('tier'),
            "severity": c.get('severity'),
            "temporal_offset_days": c.get('temporal_offset_days'),
            "corroborating_link_id": c.get('corroborating_link_id'),
            "corroborating_link_confidence": c.get('corroborating_link_confidence'),
            "corroborating_entity_type": c.get('corroborating_entity_type'),
        })

    with open(out_path, 'w') as f:
        json.dump(rows_out, f, indent=2, default=str)
    print(f"wrote {len(rows_out)} row(s) to {out_path}")


# ──────────────────────────────────────────────────────────────────────────
# Default mode: the funnel measurement (unchanged from the pre-29-08 script,
# D-04-updated to read located_at directly by contract).
# ──────────────────────────────────────────────────────────────────────────

def page(fields, table, where=""):
    out, start = [], 0
    while True:
        rows = db.query(f"SELECT id, {fields} FROM {table} {where} ORDER BY id LIMIT 20000 START {start};")
        if not rows:
            break
        out.extend(rows)
        start += len(rows)
    return out


GATE1_KEY_BIN = 'target/debug/gate1-address-key'
GATE1_KEY_BUILD = 'cargo build -p urban-integrity-enricher --bin gate1-address-key'


def gate1_keys(pairs):
    """Compute gate-1's (postcode, paon, street) key for each (location_postcode, address)
    pair using the SHIPPED Rust implementation, never a Python re-derivation.

    This script previously computed the key itself, with a regex PAON and a whole-address
    `norm_street`. That made it a SECOND address parser — and when 29-08 reported
    `candidates_written=0`, this script independently reported zero too, which was recorded
    as cross-validation of an "honest structural finding". It was no such thing: both
    implementations shared the same wrong assumption, and the real defect was a parser bug
    that made full-triple matches impossible for the corpus's dominant address shape. Two
    implementations agreeing is not independent evidence — it is one bug counted twice.

    The Rust side owns the whole triple, postcode included, because `normalise_postcode`'s
    length and alphanumeric validation is part of the key. A Python `.upper().replace(' ','')`
    accepts postcodes gate-1 rejects, which is the same divergence one level down.

    See planning open item 13 and the phase 29 verification report.
    """
    if not pairs:
        return []
    if not os.path.exists(GATE1_KEY_BIN):
        sys.exit(f"{GATE1_KEY_BIN} not built — run:\n  {GATE1_KEY_BUILD}")

    def clean(v):
        # Tabs and newlines are the record/field separators — neutralise them so a rogue
        # address cannot desynchronise the stream against its inputs.
        return (v or '').replace('\t', ' ').replace('\n', ' ').replace('\r', ' ')

    payload = ''.join(f"{clean(pc).strip()}\t{clean(addr)}\n" for pc, addr in pairs)
    r = subprocess.run([GATE1_KEY_BIN], input=payload, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"{GATE1_KEY_BIN} failed (exit {r.returncode}): {r.stderr[:400]}")
    out = [json.loads(line) for line in r.stdout.splitlines()]
    if len(out) != len(pairs):
        # Positional zip is the contract; a length mismatch means results are being
        # attributed to the wrong addresses. Fail loudly rather than report nonsense.
        sys.exit(f"{GATE1_KEY_BIN} returned {len(out)} rows for {len(pairs)} inputs — alignment broken")
    return out


def run_funnel() -> None:
    con = page("entity_id, application_ref, application_received_date", "contract", "WHERE application_ref != NONE")
    aw = page("in.entity_id AS applicant, out.entity_id AS contract", "awarded_to")
    # POST-fix (D-04): located_at is contract-sourced — read it directly by
    # contract, matching 29-04's detector-side join. No awarded_to chaining
    # needed for the location lookup any more.
    lo = page("in.entity_id AS contract, out.entity_id AS location", "located_at")
    loc = page("entity_id, postcode, name", "location")
    ppd = page("postcode_norm, paon_norm, street_norm", "ppd_transactions")
    print(f"contract(app_ref)={len(con)}  awarded_to={len(aw)}  located_at={len(lo)}  location={len(loc)}  ppd={len(ppd)}")

    # Kept only for whatever gate-3-adjacent reporting still needs the
    # contract -> applicant relationship (e.g. the "has a resolved applicant"
    # gate below) — no longer used to key the location lookup.
    aw_m = {s(r['contract']): s(r['applicant']) for r in aw}
    # The detector keeps exactly ONE location per contract — `HashMap::insert` in its scan
    # loop, so the LAST `located_at` row wins — and skips the application if that one location
    # has no usable key. Collecting a set and taking the first usable member measured a
    # different thing, and did so nondeterministically: str hashing is seeded per process, so
    # any contract with more than one location could change the totals between runs of the
    # same data. `lo` is consumed in scan order, so last-write-wins here matches the detector.
    lo_one = {}
    for r in lo:
        c, l = s(r['contract']), s(r['location'])
        if c and l:
            lo_one[c] = l
    locm = {s(r['entity_id']): (r.get('postcode'), r.get('name') or '') for r in loc}
    P = {(r['postcode_norm'], r['paon_norm'], r['street_norm']) for r in ppd}
    Ppc = {r['postcode_norm'] for r in ppd}

    # Resolve every location's gate-1 key up front, in ONE call to the shipped Rust
    # implementation, then look them up positionally below. Batched because the cost is
    # per-process, not per-address.
    loc_ids = [lid for lid in locm]
    keys = gate1_keys([locm[lid] for lid in loc_ids])
    keym = dict(zip(loc_ids, keys))

    n_appl = n_loc = n_full = 0
    pc_hit = shipped = 0
    locs_per_applicant = collections.Counter()
    for c in {s(r['entity_id']) for r in con}:
        a = aw_m.get(c)
        if not a:
            continue
        n_appl += 1
        l = lo_one.get(c)
        locs_per_applicant[1 if l else 0] += 1
        best = False
        if l:
            n_loc += 1
            k = keym.get(l)
            # `usable` is gate-1's own precondition (all three components non-empty),
            # evaluated by the detector's code rather than guessed at here. An unusable key
            # ends this application, exactly as it does in the detector — no falling through
            # to another location.
            if k and k['usable']:
                pcn, pn, st = k['postcode'], k['paon'], k['street']
                best = True
                if pcn in Ppc:
                    pc_hit += 1
                if (pcn, pn, st) in P:
                    shipped += 1
        if best:
            n_full += 1

    print()
    print(f"applications with an applicant            : {n_appl}")
    print(f"  ...applicant has >=1 located_at         : {n_loc}")
    print(f"  ...address yields all 3 components      : {n_full}")
    print(f"gate-1 postcode present in PPD            : {pc_hit}")
    print(f"gate-1 full triple (shipped Rust parser)  : {shipped}")
    print()
    # Now 0/1 by construction: the detector keeps one location per contract, so this reports
    # how many applications have one at all, not how many the raw edge table holds.
    print("applications with a location (0/1):", dict(sorted(locs_per_applicant.items())))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--fp-sample", metavar="PATH", help="write a stratified FP-adjudication sample to PATH")
    args = parser.parse_args()
    if args.fp_sample:
        fp_sample(args.fp_sample)
    else:
        run_funnel()
