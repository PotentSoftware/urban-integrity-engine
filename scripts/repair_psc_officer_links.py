#!/usr/bin/env python3
"""Repoint the `ch_psc` identity links whose officer target does not exist.

WHY (planning open item 33). `match_officer` used to return whichever *lookup key* had
matched, that key was written straight into `target_node_id`, and `build_identity_link`
recovered the match class by testing the same string for an `OFF:` prefix — one string
doing two jobs. PR #38 re-keyed every officer node to `director:{slug}` and moved the
company scope into `OfficerData::company_number`; nothing repointed the links. The code
path is fixed (`7ebbca9`) so no NEW link can dangle, but the existing rows were left
pointing at ids that exist in neither the `officer` table nor `graph_events`.

Measured live on `an_anglesey_aebaa785` (2026-08-16): 13,519 of 13,547 `ch_psc` links
dangle — all 6,666 `off_composite`, and 6,853 of 6,881 `director_slug`.

WHAT THIS DOES. It does NOT re-derive matches from Companies House. Every link already
carries the two facts needed to find its real target, in `evidence_json`:

    {"name_slug": "john-smith", "company_number": "01234567", ...}

so the repair is a local join against the `officer` table:

  1. `name_slug` is looked up against every officer's name slug in BOTH orderings —
     `officer_name_slugs` in `ch_psc.rs`, mirrored here. PSC slugs are forename-first
     (`john-smith`); officer nodes are keyed from CH's "SURNAME, Forenames"
     (`smith-john`). Matching one ordering only is the second reason these dangle.
  2. Exactly one officer matches  ->  repoint.
  3. Several match  ->  the `company_number` must pick exactly one of them, via the
     company that officer is joined to by a `directs` edge (or `officer.company_number`).
  4. Still several  ->  NOT repaired. Flagged `pending_review` instead.

  5. Two links can want the SAME destination — the defect wrote one `off_composite` and one
     `director_slug` row per PSC, the same assertion under two stale class labels. Only one
     survives; the rest are DELETED, because `identity_links` carries a UNIQUE index on
     `(source_node_id, target_node_id, source)` and repointing both is rejected by it.

Step 4 is the whole point of the script's caution. Auto-merging two same-named officers
is exactly the class of error Open Item 21 spent a day undoing, and one of those fused
nodes carried the project's only Phoenix alert — `T-23-FalseAccusation`, the gravest
failure mode this system has. A dangling link is inert; a wrongly-repointed one accuses
someone. So an ambiguous link is left unrepaired AND demoted out of `auto_linked`,
because `auto_linked` on a link that points nowhere is an assertion the data cannot back.

MATCH CLASS IS RECOMPUTED, not carried over. `resolution_type` (and `evidence_json.match`)
is re-derived from whether the repaired target is actually company-corroborated. The
stored classes are not trustworthy: they were produced by the id-prefix test above, so on
this corpus 6,645 links recorded as name-only `director_slug` are in fact directed by the
PSC's own company, and were only classed name-only because the old consumer could not
read `company_number` at all.

WHAT CHANGES AS A RESULT. `identity_links` is NOT read by the detector — nothing in
`crates/detector` touches it. It is loaded as synthetic traversal edges by
`core::persistence::traversal::bfs_traversal` (`:301`, gated to `auto_linked`/`confirmed`),
which is the DB-backed path behind `graph-path`, `entity` and GraphQL. So this repair takes
effect the moment it is applied and needs NO bake and NO re-projection; what it fixes is
13,519 virtual edges that traversed to a node that was not there.

SAFETY. Writes only to `identity_links`, and only to rows whose `source = 'ch_psc'`. The
only rows DELETED are duplicates of an assertion another row is keeping: a group shares a
`source_node_id` (which embeds the company number) and a destination officer, so every
member recomputes to the same class and the same evidence, and merging them loses nothing.
Deletions run BEFORE any repoint, because a duplicate is deleted precisely to free the
unique-index triple a survivor is about to occupy. `graph_events` is not
touched, so no re-projection is required — but a bake is, if you want alerts to reflect
the new links. Dry-run by default. Every statement is checked for a returned row, because
SurrealDB reports `status: OK` with an empty result for an UPDATE that matched nothing —
the failure mode that let the ICIJ adapter report 814k writes into an empty table. Note
that a batch has already executed in full by the time its responses are read, so an abort
stops the NEXT batch rather than undoing the last one; the script is re-runnable and simply
re-reads the live table, seeing completed work as already valid.

Usage:
  python3 scripts/repair_psc_officer_links.py --analysis <analysis-id>
  python3 scripts/repair_psc_officer_links.py --analysis <analysis-id> --apply
"""
from __future__ import annotations

import argparse
import collections
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import surreal_target

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLAN = os.path.join(ROOT, "data", "psc-officer-link-repair-plan.tsv")

PAGE = 5000
BATCH = 200


# ── name slugs: mirrors crates/enricher/src/adapters/ch_psc.rs ──────────────────────────

def normalise_name(name: str) -> str:
    """Mirror of `core::stream::normalise::normalise_name`."""
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


# The two functions above are hand-mirrors of Rust that lives in another crate, and a silent
# drift between them would make this script repoint links onto the wrong officers. These are
# the cases the Rust unit tests pin (`ch_psc.rs` officer_slugs_*, `normalise.rs` test_normalise_*),
# asserted at import so drift fails loudly instead of quietly changing the plan.
_SELF_CHECK = [
    ("ACME Ltd", ["acme"]),
    ("Apex Build Limited", ["apex-build"]),
    ("O'Brien Construction Ltd.", ["obrien-construction"]),
    ("Well-Being Ltd", ["wellbeing"]),
    ("Smith & Jones LLP", ["smith-jones"]),
    ("Ltd", []),
    ("EXAMPLE, Jane Alexandra", ["example-jane-alexandra", "jane-alexandra-example"]),
    ("Jane Alexandra Example", ["jane-alexandra-example"]),
    ("DE SAMPLE, Jordan, Dr", ["de-sample-jordan-dr", "jordan-dr-de-sample"]),
    ("SMITH,", ["smith"]),
]

for _name, _expected in _SELF_CHECK:
    _got = officer_name_slugs(_name)
    assert _got == _expected, f"slug mirror drifted from ch_psc.rs: {_name!r} -> {_got} != {_expected}"


# ── reads ──────────────────────────────────────────────────────────────────────────────

def page_all(target, template: str, timeout: int = 300) -> list:
    """Page a SELECT by START/LIMIT, advancing by rows RECEIVED.

    Advancing by the page size requested treats a short page as end-of-data — the defect
    fixed across six loops in `063700f`. This advances by `len(rows)` instead.
    """
    rows, start = [], 0
    while True:
        resp = target.query(template.format(start=start, size=PAGE), timeout=timeout)
        block = resp[0] if resp else {}
        result = block.get("result")
        if block.get("status") != "OK" or not isinstance(result, list):
            sys.exit(f"error: query failed: {block.get('result')}")
        rows.extend(result)
        if len(result) < PAGE:
            return rows
        start += len(result)


def load_officers(target):
    rows = page_all(
        target,
        "SELECT entity_id, name, company_number FROM officer "
        "ORDER BY entity_id START {start} LIMIT {size};",
    )
    existing = {r["entity_id"] for r in rows}
    index = collections.defaultdict(set)
    for r in rows:
        for slug in officer_name_slugs(r.get("name") or ""):
            index[slug].add(r["entity_id"])
    return rows, existing, index


def load_officer_companies(target, officers):
    """officer entity_id -> set of company numbers it is scoped to.

    Two sources, unioned: the `directs` edge (company -> officer), which covers every
    officer, and `OfficerData::company_number` on the node, which PR #38 added and which
    only the 1,299 officers written since then carry.
    """
    scope = collections.defaultdict(set)
    edges = page_all(
        target,
        "SELECT id, record::id(in) AS company, record::id(out) AS officer "
        "FROM directs ORDER BY id START {start} LIMIT {size};",
    )
    for e in edges:
        company = e.get("company") or ""
        if company.startswith("GB-COH:"):
            scope[e["officer"]].add(company[len("GB-COH:"):])
    for o in officers:
        if o.get("company_number"):
            scope[o["entity_id"]].add(o["company_number"])
    return scope, len(edges)


def load_links(target):
    return page_all(
        target,
        "SELECT id, source_node_id, target_node_id, resolution_type, evidence_json, status "
        "FROM identity_links WHERE source = 'ch_psc' "
        "ORDER BY id START {start} LIMIT {size};",
    )


# ── planning ───────────────────────────────────────────────────────────────────────────

def resolve_target(link, existing, index, scope):
    """The officer id this link SHOULD point at, plus why. `None` target = not repairable."""
    current = link["target_node_id"]
    if current in existing:
        return current, "already_valid"
    evidence = json.loads(link.get("evidence_json") or "{}")
    slug = evidence.get("name_slug") or ""
    company = evidence.get("company_number") or ""
    candidates = sorted(index.get(slug, ()))
    if not candidates:
        return None, "no_officer_matches_the_name"
    if len(candidates) == 1:
        return candidates[0], "unique_by_slug"
    corroborated = [c for c in candidates if company and company in scope.get(c, ())]
    if len(corroborated) == 1:
        return corroborated[0], "unique_by_company"
    return None, f"ambiguous_{len(candidates)}"


def build_plan(links, existing, index, scope):
    """Decide, for every `ch_psc` link, whether to repoint it, merge it away, or flag it.

    THE MERGE STEP IS NOT OPTIONAL. `identity_links` carries a UNIQUE index on
    `(source_node_id, target_node_id, source)` (`idx_identity_links_unique`, `schema/db.rs`),
    and the defect being repaired routinely produced TWO rows per PSC — one `off_composite`
    keyed `OFF:{company}-{slug}` and one `director_slug` keyed `director:{psc-slug}` — which
    are the same assertion under two stale class labels. Repointing both lands them on the
    same officer and the second UPDATE is rejected by the index. Measured live: the first
    `--apply` attempt died on exactly that, at the first PSC whose two stale rows collided.

    So each destination triple keeps exactly ONE row and the rest are deleted. This loses no
    information: the group shares a `source_node_id` (which embeds the company number) and a
    target, so every member recomputes to the same class and the same evidence. The survivor
    is a row already sitting at the destination where one exists — that row needs no write at
    all — otherwise the lexicographically smallest id, so the choice is deterministic and a
    re-run picks the same one.
    """
    plan, stats = [], collections.Counter()

    # Pass 1: where every link wants to end up.
    finals = {}
    for link in links:
        chosen, how = resolve_target(link, existing, index, scope)
        finals[link["id"]] = (chosen, how)
        stats[how] += 1

    # Pass 2: one survivor per destination triple.
    groups = collections.defaultdict(list)
    for link in links:
        chosen, _how = finals[link["id"]]
        if chosen is None:
            continue
        groups[(link["source_node_id"], chosen)].append(link)

    survivors = {}
    for key, members in groups.items():
        settled = [m for m in members if m["target_node_id"] == key[1]]
        winner = min(settled or members, key=lambda m: m["id"])
        survivors[key] = winner["id"]

    # Pass 3: the actions.
    for link in links:
        chosen, how = finals[link["id"]]
        evidence = json.loads(link.get("evidence_json") or "{}")
        company = evidence.get("company_number") or ""
        slug = evidence.get("name_slug") or ""

        if chosen is None:
            # Dangling and unrepairable. `auto_linked` on a link pointing nowhere is an
            # assertion the data cannot back, so it is demoted for a human.
            plan.append((link, "flag_review", "", how, company, slug))
            continue

        keeper = survivors[(link["source_node_id"], chosen)]
        if keeper != link["id"]:
            stats["merged_duplicate"] += 1
            plan.append((link, "merge_duplicate", keeper, how, company, slug))
            continue

        if how == "already_valid":
            stats["already_valid_untouched"] += 1
            continue

        klass = "off_composite" if company and company in scope.get(chosen, ()) else "director_slug"
        stats[f"class_{klass}"] += 1
        if klass != link["resolution_type"]:
            stats["class_changed"] += 1
        plan.append((link, "repoint", chosen, how, company, slug))

    return plan, stats


def write_plan(plan):
    os.makedirs(os.path.dirname(PLAN), exist_ok=True)
    with open(PLAN, "w") as f:
        f.write("link_id\taction\tsource_node_id\told_target\tnew_target\tbasis\t"
                "company_number\tname_slug\told_class\tnew_class\n")
        for link, action, chosen, how, company, slug in plan:
            new_class = ""
            if action == "repoint":
                new_class = link["_new_class"]
            f.write(
                f"{link['id']}\t{action}\t{link['source_node_id']}\t{link['target_node_id']}\t"
                f"{chosen}\t{how}\t{company}\t{slug}\t{link['resolution_type']}\t{new_class}\n"
            )


# ── writes ─────────────────────────────────────────────────────────────────────────────

def sq(value: str) -> str:
    """Single-quote a SurrealQL string literal."""
    return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"


def rid(link_id: str) -> str:
    """`identity_links:abc` -> a record literal that survives odd id charsets."""
    table, _, key = link_id.partition(":")
    return f"{table}:`{key}`"


def _run_batch(target, chunk, statements, label):
    """Send one batch and verify every statement returned the row it claimed to write.

    NOTE ON PARTIAL APPLICATION. SurrealDB executes every statement in the batch before
    this sees any response, so an abort here stops the NEXT batch — it does not undo the
    one that just ran. That is safe because each statement stands alone and the whole
    script is re-runnable (a repointed row simply reads back as already valid), but the
    count reported on abort has to say how many statements RAN, not how many succeeded,
    or it understates what changed. The first `--apply` attempt reported "aborting after
    146 rows" when 199 of the 200-statement batch had in fact applied.
    """
    resp = target.query("\n".join(statements), timeout=300)
    if len(resp) != len(chunk):
        sys.exit(
            f"error: sent {len(chunk)} {label} statements, got {len(resp)} responses — "
            f"the batch has already run; re-run the script to see the resulting state"
        )
    for (link, *_), block in zip(chunk, resp):
        result = block.get("result")
        if block.get("status") != "OK" or not result:
            sys.exit(
                f"error: {label} {link['id']} matched no row "
                f"(status={block.get('status')}, result={result!r}).\n"
                f"       The {len(chunk)}-statement batch containing it HAS run; nothing is\n"
                f"       rolled back. Re-run the script — it re-reads the live table and\n"
                f"       replans, so completed work is simply seen as already valid."
            )


def apply_plan(target, plan):
    """Apply the plan: duplicate deletions FIRST, then repoints, then review flags.

    Order is load-bearing. `idx_identity_links_unique` covers
    `(source_node_id, target_node_id, source)`, and a duplicate row is deleted precisely
    because a survivor is about to occupy its triple. Repointing before deleting would hit
    the index the delete exists to clear.
    """
    merges = [p for p in plan if p[1] == "merge_duplicate"]
    repoints = [p for p in plan if p[1] == "repoint"]
    flags = [p for p in plan if p[1] == "flag_review"]

    for i in range(0, len(merges), BATCH):
        chunk = merges[i : i + BATCH]
        _run_batch(
            target,
            chunk,
            [f"DELETE {rid(link['id'])} RETURN BEFORE;" for link, *_ in chunk],
            "DELETE",
        )

    for i in range(0, len(repoints), BATCH):
        chunk = repoints[i : i + BATCH]
        statements = []
        for link, _action, chosen, *_ in chunk:
            new_class = link["_new_class"]
            evidence = json.loads(link.get("evidence_json") or "{}")
            evidence["match"] = new_class
            statements.append(
                f"UPDATE {rid(link['id'])} SET "
                f"target_node_id = {sq(chosen)}, "
                f"resolution_type = {sq(new_class)}, "
                f"evidence_json = {sq(json.dumps(evidence, separators=(',', ':'), sort_keys=True))} "
                f"RETURN id;"
            )
        _run_batch(target, chunk, statements, "UPDATE")

    for i in range(0, len(flags), BATCH):
        chunk = flags[i : i + BATCH]
        _run_batch(
            target,
            chunk,
            [f"UPDATE {rid(link['id'])} SET status = 'pending_review' RETURN id;"
             for link, *_ in chunk],
            "UPDATE",
        )

    return len(repoints), len(merges), len(flags)


# ── main ───────────────────────────────────────────────────────────────────────────────

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    surreal_target.add_analysis_arg(parser)
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Perform the writes. Without it this is a read-only dry run.",
    )
    args = parser.parse_args()

    target = surreal_target.resolve(args)
    print(target.banner())
    target.preflight()

    officers, existing, index = load_officers(target)
    scope, directs = load_officer_companies(target, officers)
    links = load_links(target)
    print(
        f"read: {len(officers)} officers, {directs} directs edges, {len(links)} ch_psc links"
    )

    plan, stats = build_plan(links, existing, index, scope)

    # Stamp the recomputed class onto each repoint row for the plan file and the writes.
    for link, action, chosen, how, company, slug in plan:
        if action == "repoint":
            link["_new_class"] = (
                "off_composite" if company and company in scope.get(chosen, ()) else "director_slug"
            )

    dangling = sum(1 for l in links if l["target_node_id"] not in existing)
    repoints = sum(1 for p in plan if p[1] == "repoint")
    merges = sum(1 for p in plan if p[1] == "merge_duplicate")
    flags = sum(1 for p in plan if p[1] == "flag_review")
    print(
        f"dangling: {dangling}  repoint: {repoints}  "
        f"merge into an existing row: {merges}  withheld for review: {flags}"
    )
    for key in sorted(stats):
        print(f"  {key}: {stats[key]}")

    write_plan(plan)
    print(f"plan written: {os.path.relpath(PLAN, ROOT)}")

    if not args.apply:
        print("\nDRY RUN — nothing written. Re-run with --apply to perform the repair.")
        return 0

    did_repoint, did_merge, did_flag = apply_plan(target, plan)
    print(f"applied: {did_repoint} repointed, {did_merge} duplicates deleted, "
          f"{did_flag} flagged pending_review")

    # Re-verify against the database rather than trusting the counters above.
    after = load_links(target)
    still = [l for l in after if l["target_node_id"] not in existing]
    print(f"verify: {len(after)} ch_psc links remain, {len(still)} still dangling "
          f"(expected {len(links) - did_merge} and {flags})")
    if len(still) != flags or len(after) != len(links) - did_merge:
        print("error: post-repair state does not match the plan", file=sys.stderr)
        return 1
    print("Done — no bake and no re-projection needed. `graph_events` was not touched, and "
          "`identity_links` reaches the graph as synthetic traversal edges "
          "(traversal.rs:301), not through the detector, which never reads this table.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
