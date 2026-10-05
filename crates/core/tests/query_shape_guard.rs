//! Workspace guard against the two SurrealDB 3.x query shapes that lose rows silently.
//!
//! This is a source-level test rather than a behavioural one, deliberately. Both defects
//! under-report *successfully* — the query returns `Ok`, with fewer rows than exist, and no
//! caller has any signal that anything was missed. There is no runtime assertion that catches
//! them; the only reliable defence is to make the shape itself impossible to write.
//!
//! The project has paid for this three times:
//!
//! - the CH backfill ran against 18% of its input set (6,033 rows where `count()` said 33,046)
//! - `ch_psc` enriched 0.27% of its candidate companies (39 rows where `count()` said 14,198)
//!   and recorded `candidates_found: 0` while reporting success
//! - `officer`/`psc` tables sat empty with no error anywhere in the logs
//!
//! ## The two rules
//!
//! **Rule 1 — indexed `kind` paired with an unindexed predicate.** `kind` is indexed
//! (`idx_graph_events_kind`). Pairing that equality with a `CONTAINS` on the unindexed
//! `payload.*` makes the planner take the index path and return a truncated, run-to-run
//! non-deterministic row set. `count()` on the identical predicate stays correct, which is what
//! makes it so hard to spot.
//!
//! **Rule 2 — a large bare `LIMIT` with no paging.** A single large `LIMIT` returns well below
//! the requested count when the rows are big, so the query samples a fraction of the table and
//! looks like it read everything.
//!
//! ## Fixing a violation
//!
//! Use `urban_integrity_core::persistence::scan` (or the ingestor's
//! `backfill::scan_events_by_type` where a re-authenticating session is needed). Both keep `kind`
//! out of the `WHERE` clause, filter in Rust, and page by rows *actually received*.
//!
//! If a site is genuinely safe, add it to `ALLOWLIST` with a justification. An unexplained
//! exemption is the failure mode this test exists to prevent, so entries require a reason
//! string — "not fixed" must never be indistinguishable from "not noticed".

use std::path::{Path, PathBuf};

/// Sites exempt from the rules: `(path, query_fragment, reason)`.
///
/// Keyed by a distinctive fragment of the query rather than a line number — line numbers rot on
/// every edit above them.
///
/// The reason is a real field, not a convention. It used to be documented as required while the
/// tuple had no slot for it, so an exemption could be added with no way to say why — the failure
/// mode this guard exists to prevent, reproduced inside the guard. `every_allowlist_entry_is_
/// justified_and_live` below rejects an empty reason and a fragment that no longer matches
/// anything, so a stale exemption fails loudly instead of quietly widening the hole.
const ALLOWLIST: &[(&str, &str, &str)] = &[
    (
        "crates/core/tests/surreal_integration.rs",
        "kind = 'NodeAdded' AND payload.weight_json CONTAINS $needle",
        "Deliberate demonstration, not a read.      `test_query_entity_by_ch_number_regression_planner_bug` issues the buggy shape and the      fixed shape against the same seeded data and asserts they DISAGREE on row count — that      disagreement is the bug signature, and it is what catches a future edit reinstating the      kind equality in `query_entity_by_ch_number`. Rewriting this query would delete the test.",
    ),
    (
        "crates/core/src/persistence/scan.rs",
        "SELECT id FROM identity_links {where_clause} LIMIT 1",
        "identity_link_exists returns a bool, not a row set: `Ok(!rows.is_empty())`. One matching row fully answers 'does at least one exist', so discarding rows past the first is not row-loss, it is the entire semantics of an existence check — there is no coherent 'fix' that returns more rows without changing what the function means. Plan 31-02 deliberately gave this its own function rather than an overloaded `limit` parameter on scan_identity_links, exactly so an existence check could never be mistaken for, or reported as, a count (the research notes' Q3, Pitfall 3). The WHERE clause is equality-only on status/source with no CONTAINS, so Rule 1's planner-truncation mechanism does not apply either.",
    ),
    (
        "crates/core/src/persistence/scan.rs",
        "SELECT confidence FROM identity_links WHERE status = 'pending_review'",
        "Deliberate pre-fix baseline, not a read. `db_identity_links_scan_pages_past_a_hard_coded_ceiling` issues this exact ORDER BY confidence DESC LIMIT 40 shape and asserts it returns exactly 40 rows and omits the seeded low-confidence marker row, then asserts the new paged scan_identity_links_paged returns all of them including the marker. That disagreement between old and new is the evidence the test exists to produce — rewriting the old-shape query to the fixed form would delete the A/B comparison, leaving nothing proving the paged read is actually better. Same category as this file's sole pre-existing ALLOWLIST entry above.",
    ),
    (
        "crates/core/src/persistence/scan.rs",
        "SELECT id, created_at FROM land_flip_candidates WHERE status = 'pending_review'",
        "Deliberate pre-fix baseline, not a read. `db_land_flip_candidates_scan_pages_past_a_hard_coded_ceiling` issues this exact ORDER BY created_at DESC LIMIT 40 shape against 150 seeded rows, asserts it returns exactly the 40-row ceiling, then compares against the new paged scan_land_flip_candidates_paged to prove the fixed read reaches rows beyond that ceiling. Rewriting the old-shape query to the fixed form would delete the A/B comparison the test exists to produce. Same category as this file's sole pre-existing ALLOWLIST entry above.",
    ),
    (
        "crates/cli/src/commands/links.rs",
        "SELECT resolution_type, confidence FROM identity_links",
        "Deliberate pre-fix baseline, not a read. `fetch_pending_review_returns_every_row_past_the_old_confidence_ceiling` issues this exact pre-fix ORDER BY confidence DESC LIMIT 100 shape verbatim (Open Item 11c's defect, live), asserts it returns exactly the 100-row ceiling and omits the seeded lowest-confidence marker row, then asserts the fixed fetch_pending_review surfaces it. Rewriting the old-shape query would delete the evidence that the fix changes real behaviour, not just its own code path. Same category as this file's sole pre-existing ALLOWLIST entry in surreal_integration.rs.",
    ),
    (
        "crates/core/src/persistence/oracle.rs",
        "SELECT count() FROM (SELECT payload.entity_id AS entity_id FROM graph_events",
        "Row-materialising inner SELECT inside a count() aggregate, exempted by explicit assessment rather than by construction (CR-01, phase 38 code review). This is `count_distinct_entities`, the dedup-by-entity primitive: the inner SELECT groups by payload.entity_id so a re-ingested entity counts once, and the outer count() counts the groups. The count( exemption in broken_rule was narrowed to exclude nested selects precisely so this query would become visible here instead of being silently waved through. WHY IT IS ACCEPTED, and the limits of that acceptance: the figures were cross-checked live on the maintainers' reference analysis (2026-09-01) against a THIRD, unrelated query shape -- array::len(array::distinct(...)) -- which agreed exactly at 39,860 for the planning:-prefixed form and 61,756 for the Location form. That agreement is a SAMPLE, NOT A PROOF: the planner bug is explicitly run-to-run non-deterministic, so two agreeing observations cannot establish immunity. Re-run the third-shape cross-check before relying on this oracle for a new corpus, a new SurrealDB version, or a materially larger graph. If the oracle ever under-counts, the failure presents as declared > oracle -- i.e. 'the probe over-counts' -- and the recorded precedent (c8fb0c7) is that the probe is what gets changed, so a correct probe could be 'fixed' to match a truncated oracle. That asymmetry is the reason this entry states its evidence rather than merely asserting safety.",
    ),
    (
        "crates/core/src/persistence/oracle.rs",
        "AS v FROM graph_events",
        "Row-materialising inner SELECT inside a count() aggregate, same class and same assessment as the count_distinct_entities entry above (CR-01, phase 38 code review). This is `count_distinct_edge_field`, which counts distinct payload.src_id or payload.dst_id for one edge marker; it groups by one side only and never inspects the other. Accepted on the same live third-shape cross-check on the maintainers' reference analysis (2026-09-01), and under the same limitation: a sample, not a proof, to be re-run for a new corpus or SurrealDB version. See the entry above for the full reasoning and the under-count failure mode.",
    ),
    (
        "crates/cli/src/commands/flips.rs",
        "SELECT ppd_transaction_id, created_at FROM land_flip_candidates",
        "Deliberate pre-fix baseline, not a read. `fetch_pending_review_returns_every_row_past_the_old_created_at_ceiling` issues this exact pre-fix ORDER BY created_at DESC LIMIT 100 shape verbatim against 150 seeded rows spanning a date range, asserts it returns exactly the 100-row ceiling and omits the oldest row's marker transaction id, then asserts the fixed fetch_pending_review surfaces it. Rewriting the old-shape query would delete the evidence that the fix changes real behaviour, not just its own code path. Same category as this file's sole pre-existing ALLOWLIST entry in surreal_integration.rs.",
    ),
];

/// The measured pre-Phase-32 `ALLOWLIST` count: 1 pre-existing `surreal_integration.rs` entry
/// plus the 5 added by 31-08. This is Phase 32's ROADMAP success criterion 1 bar — "zero new
/// ALLOWLIST entries across the entire probe battery tree" (then `crates/cli/src/commands/probe/`,
/// now `crates/probe/src/` after plan 43-01).
///
/// A green `every_allowlist_entry_is_justified_and_live` run does NOT prove this on its own: it
/// passes happily against a newly added, properly justified entry, because its job is checking
/// that every entry that DOES exist is live and explained, not that the count stayed put.
/// `allowlist_gained_no_entries_this_phase` below is the test that actually falsifies "zero new
/// entries" — see the plan 32-06 summary for the observed failure this test produced when a dummy
/// 7th entry was added during development, and then removed.
/// Raised 6 -> 8 on 2026-09-01 by explicit operator decision, recorded here and in
/// the phase 38 code review (CR-01). The two new entries are `persistence::oracle`'s dedup primitives. They
/// are NOT a silent widening: they became visible only because `broken_rule`'s `count(` exemption
/// was deliberately NARROWED in the same change, so the net effect is that two previously
/// invisible Rule-1 shapes are now recorded, justified and re-checkable. Each entry's reason
/// carries the live cross-check evidence and states explicitly that it is a sample rather than a
/// proof.
const ALLOWLIST_BASELINE: usize = 8;

/// Phase 32's falsifiable version of ROADMAP success criterion 1's "zero new ALLOWLIST entries"
/// bar. If a probe genuinely cannot avoid a guard-flagged shape, the fix is an explicit operator
/// decision recorded in a PLAN (as 31-08's five entries were) — not a silent widening of this
/// list.
#[test]
fn allowlist_gained_no_entries_this_phase() {
    assert_eq!(
        ALLOWLIST.len(),
        ALLOWLIST_BASELINE,
        "ROADMAP success criterion 1 requires zero new ALLOWLIST entries across the entire \
         probe battery tree (crates/probe/src/). ALLOWLIST has {} entries, expected the \
         pre-Phase-32 baseline of {ALLOWLIST_BASELINE}. A genuinely necessary new entry needs an \
         explicit operator decision recorded in a PLAN, exactly as 31-08's five entries were — \
         it must never be added silently just to make this test pass.",
        ALLOWLIST.len()
    );
}

/// Tables this guard watches: `(table name, Rule-2 "unpaged read is suspect" LIMIT threshold,
/// indexed fields Rule 1 checks for a dangerous equality+CONTAINS pairing)`.
///
/// `push_if_sql` and `broken_rule` both drive off this single list. Before this widening, the two
/// functions each hard-coded `"from graph_events"` independently (`:133`/`:243`), a drift trap
/// that had to be kept in sync by hand — closed by construction now, since there is exactly one
/// place a table can be added.
///
/// Two thresholds are deliberately NOT the default `1` — read their comments before "fixing"
/// them back down; both were chosen to avoid flagging a genuinely safe, unique-key point lookup.
const WATCHED_TABLES: &[(&str, u64, &[&str])] = &[
    // Unchanged from the original single-table guard — sized for `weight_json` response-size
    // risk (`scan.rs:14-15`): a single large response can itself truncate well under a
    // full-table read, which is a different risk shape from the other five tables below.
    ("graph_events", 1_000, &["kind"]),
    // Any unpaged sample on this table is suspect (the research notes' Q2); PRB-04/GRD-03/GRD-09 require
    // every identity_links read fixed, not sized to a "large" magnitude.
    (
        "identity_links",
        1,
        &["source_node_id", "target_node_id", "status"],
    ),
    // Zero indexes defined on this table (`db.rs:238`, `DEFINE TABLE … SCHEMALESS` with no
    // `DEFINE FIELD`/`DEFINE INDEX`) — Rule 1's mechanism (the planner taking an index path) has
    // no index path to misuse here, so the indexed-field list is deliberately empty; a Rule-1
    // fixture on this table would demonstrate nothing. No real code currently issues a
    // bare-LIMIT SELECT on this table either, so the low threshold costs nothing today.
    ("enrichment_runs", 1, &[]),
    // No real code currently issues a bare-LIMIT SELECT here either (`opencorporates.rs:122` has
    // no LIMIT at all).
    ("oc_quota", 1, &["year_month"]),
    // GRD-09 explicitly names `flips.rs:72-78` as a violation on this table needing a fix, not a
    // threshold dodge.
    (
        "land_flip_candidates",
        1,
        &["status", "ppd_transaction_id", "planning_ref"],
    ),
    // DELIBERATELY kept high, NOT 1. D-16 (2026-08-27, Phase 34) assessed this shape and closed
    // it: `crates/enricher/src/land_flip.rs:606-615`'s `LIMIT 200` gate-1 join is scoped to a
    // single exact `(postcode_norm, paon_norm, street_norm)` address triple, and 200 PPD
    // transactions against one address is not a reachable state -- so the shape is not
    // row-losing and needs no paged replacement. The threshold stays `1_000` as the closure of
    // that assessment, not unfinished work. Do not "fix" this back down to 1 without reading this
    // comment first.
    (
        "ppd_transactions",
        1_000,
        &["transaction_id", "district_code", "postcode_norm"],
    ),
    // D-15 (Phase 34, plan 34-01): single-record marker table, no `DEFINE INDEX` (mirrors the
    // `enrichment_runs` entry above) -- empty indexed-field list is deliberate, not an omission.
    ("land_flip_runs", 1, &[]),
    // D-15 (Phase 34, plan 34-01): mirrors `land_flip_candidates`'s indexed-field-list shape.
    (
        "land_flip_gate1_survivors",
        1,
        &["planning_ref", "applicant_id"],
    ),
    // DELIBERATELY NOT 1. `crates/cli/src/registry.rs:102`
    // (`SELECT * FROM analyses WHERE db_name = $db_name LIMIT 1`) is a genuine unique-key point
    // lookup — `db_name` carries a UNIQUE index (`db.rs:384`), so at most one row can EVER match
    // regardless of the planner; flagging it at threshold 1 would be a false positive with no
    // real defect behind it. Threshold 2 still catches any genuine multi-row completeness read on
    // this table while leaving the one legitimate `LIMIT 1` unique lookup untouched. Do not "fix"
    // this back down to 1 without reading this comment first.
    //
    // `_catalog.analyses` matches as the bare literal `analyses` — SurrealDB never dot-qualifies
    // database.table in a FROM clause (the database is selected separately via
    // `.use_db("_catalog")`, `registry.rs:60`). Confirmed via `grep` (2026-08-23) that no other
    // word containing "analyses" exists elsewhere under `crates/`, so the bare substring match is
    // safe.
    ("analyses", 2, &["db_name"]),
];

/// The first [`WATCHED_TABLES`] entry whose table name appears as `from {table}` in `q`, which
/// must already be lowercased.
fn watched_table_for(q: &str) -> Option<(&'static str, u64, &'static [&'static str])> {
    WATCHED_TABLES
        .iter()
        .find(|(table, _, _)| q.contains(&format!("from {table}")))
        .copied()
}

/// This file, relative to the workspace root.
///
/// Exempt from its own rules: the fixtures in the unit tests below are the broken shapes, written
/// out in full so the matcher can be pinned against them.
const SELF_PATH: &str = "crates/core/tests/query_shape_guard.rs";

#[derive(Debug)]
struct Violation {
    file: String,
    rule: String,
    query: String,
}

#[test]
fn no_production_query_uses_a_row_losing_shape() {
    let root = workspace_root();
    let mut violations: Vec<Violation> = Vec::new();

    for path in rust_sources(&root) {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        // Only THIS file is exempt, because its own unit tests below hold the broken shapes
        // deliberately, as the fixtures the matcher is pinned against.
        //
        // Test paths used to be skipped wholesale, on the reasoning that regression tests need to
        // be able to write the broken shape. That is true of *demonstration* queries and false of
        // *assertion* queries: a negative test that asks "are there zero matching rows?" through
        // the row-losing shape passes when the planner bug bites, so the bug makes the test that
        // would catch it succeed. Every instance found on 2026-08-21 — three negative tests in
        // `property_accumulation.rs` and one in `evasion_detector.rs` — lived in this blind spot.
        // A genuine demonstration query belongs in ALLOWLIST with a reason.
        if rel == SELF_PATH {
            continue;
        }

        let src = std::fs::read_to_string(&path).expect("read source");
        for query in sql_literals(&src) {
            if is_allowlisted(&rel, &query) {
                continue;
            }
            if let Some(rule) = broken_rule(&query) {
                violations.push(Violation {
                    file: rel.clone(),
                    rule,
                    query: squeeze(&query, 160),
                });
            }
        }
    }

    if !violations.is_empty() {
        let mut report = format!(
            "{} production quer{} use a shape that loses rows silently.\n\n\
             Fix: route through `urban_integrity_core::persistence::scan` (or the ingestor's \
             `backfill::scan_events_by_type`).\n\
             If a site is genuinely safe, add it to ALLOWLIST in this file with a reason.\n\n",
            violations.len(),
            if violations.len() == 1 { "y" } else { "ies" }
        );
        for v in &violations {
            report.push_str(&format!("  {} [{}]\n      {}\n", v.file, v.rule, v.query));
        }
        panic!("{report}");
    }
}

/// Which rule a query breaks, if any.
fn broken_rule(query: &str) -> Option<String> {
    let q = query.to_ascii_lowercase();
    // `count()` is unaffected by the planner bug — it is the thing that stays correct while the
    // row-returning form truncates, and is often the only way to detect the problem.
    //
    // CR-01 (phase 38 code review) narrowed this exemption. The measured fact behind it is that `count()`
    // evaluated DIRECTLY over the predicate stays correct — a server-side aggregate that never
    // materialises rows. It says nothing about a `count()` wrapping a nested row-returning
    // `SELECT`, which still materialises that inner row set and is therefore exposed to exactly
    // the truncation Rule 1 exists to catch. Before this narrowing the outer aggregate's `count(`
    // substring alone bought the whole query an exemption, which is how `persistence::oracle`'s
    // two dedup primitives came to emit a verbatim Rule-1 shape that this guard could not see.
    let nested_select = q.matches("select").count() > 1;
    if q.contains("count(") && !nested_select {
        return None;
    }
    if !q.trim_start().starts_with("select") {
        return None;
    }
    let (_table, threshold, indexed_fields) = watched_table_for(&q)?;

    // Rule 1: an indexed field's equality predicate paired with an unindexed CONTAINS anywhere
    // in the query takes the index path and returns a truncated, run-to-run non-deterministic
    // row set (the planner bug this file exists to catch). Checked first — it is
    // magnitude-independent, so it must win over Rule 2/3 regardless of the LIMIT present.
    let indexed_eq_field = indexed_fields.iter().find(|f| {
        q.contains(&format!("{f} = '"))
            || q.contains(&format!("{f}=$"))
            || q.contains(&format!("{f} = $"))
    });
    if let Some(field) = indexed_eq_field {
        if q.contains("contains") {
            return Some(format!(
                "rule 1: indexed `{field}` paired with an unindexed CONTAINS"
            ));
        }
    }

    let limit = trailing_limit(&q);
    let has_order_by = q.contains("order by");
    let has_start = q.contains("start");

    // Rule 3 / GRD-08: an ordered read with a LIMIT ceiling and no START biases the sample
    // toward one end of the ordering, regardless of the table's own Rule-2 magnitude threshold —
    // checked before Rule 2 so a query that would also trip Rule 2 reports the more specific
    // diagnosis.
    if limit.is_some() && has_order_by && !has_start {
        return Some(
            "rule 3: ordered read with a LIMIT ceiling and no START (biased sample)".to_string(),
        );
    }

    // Rule 2: a large bare LIMIT with no paging, using this table's own threshold rather than a
    // single global magnitude.
    if let Some(limit) = limit {
        if limit >= threshold && !has_start {
            return Some("rule 2: large bare LIMIT with no paging".to_string());
        }
    }
    None
}

/// The numeric argument of a `LIMIT`, when it is a literal.
fn trailing_limit(q: &str) -> Option<u64> {
    let idx = q.rfind("limit ")?;
    q[idx + 6..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}

fn is_allowlisted(rel: &str, query: &str) -> bool {
    ALLOWLIST
        .iter()
        .any(|(file, fragment, _reason)| rel == *file && query.contains(fragment))
}

/// Every string literal in `src` that looks like SQL, with Rust's line-continuation escapes
/// collapsed so a query split across source lines is matched as one string.
///
/// This walks the source the way rustc's lexer does — comments, char literals and raw strings are
/// consumed as units rather than scanned for quotes. Pairing quotes naively is not a shortcut, it
/// is a silent failure: a single `'"'` char literal (`s.trim_matches('"')`, which this workspace
/// uses in a dozen places) inverts the scanner's phase, so from that point on every real query
/// reads as a gap and the code between queries reads as a literal. That is not hypothetical — it
/// hid two rule violations in `property_accumulation.rs` and three in `queries.rs` behind a green
/// test with an empty allowlist.
fn sql_literals(src: &str) -> Vec<String> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Comments — a `"` inside one is prose, and an odd number of them used to desync
        // everything below.
        if src[i..].starts_with("//") {
            i = src[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            continue;
        }
        if src[i..].starts_with("/*") {
            let mut depth = 1usize; // block comments nest in Rust
            i += 2;
            while i < bytes.len() && depth > 0 {
                if src[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if src[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Char literals (`'"'`, `'\''`) and lifetimes (`'a`) — never SQL, and the first is the
        // exact byte that used to invert the scan.
        if bytes[i] == b'\'' {
            i += char_literal_len(&src[i..]).unwrap_or(1);
            continue;
        }
        // Raw strings: `r"…"`, `r#"…"#`. The body is verbatim, so `\` is not an escape there.
        // The preceding byte must not be an identifier character, or a closure binding named
        // `…r` sitting next to a string would be read as one.
        if bytes[i] == b'r' && !i.checked_sub(1).is_some_and(|p| is_ident_byte(bytes[p])) {
            if let Some((body, len)) = raw_string(&src[i..]) {
                push_if_sql(&mut out, body);
                i += len;
                continue;
            }
        }
        if bytes[i] == b'"' {
            let (body, len) = normal_string(&src[i..]);
            push_if_sql(&mut out, &body);
            i += len;
            continue;
        }
        i += 1;
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn push_if_sql(out: &mut Vec<String>, lit: &str) {
    let lower = lit.to_ascii_lowercase();
    if WATCHED_TABLES
        .iter()
        .any(|(table, _, _)| lower.contains(&format!("from {table}")))
    {
        out.push(lit.to_owned());
    }
}

/// Byte length of the char literal starting at `s`, or `None` if this quote opens a lifetime.
fn char_literal_len(s: &str) -> Option<usize> {
    let rest = &s[1..];
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    let after_first = if first == '\\' {
        // `'\n'`, `'\''`, `'\u{1F600}'` — the closing quote sits after the whole escape, not
        // after the first quote-looking byte inside it.
        if rest.as_bytes().get(1) == Some(&b'u') {
            rest.find('}')? + 1
        } else {
            2
        }
    } else {
        first.len_utf8()
    };
    (rest.as_bytes().get(after_first) == Some(&b'\'')).then_some(1 + after_first + 1)
}

/// The body and total byte length of the raw string starting at `s`, if one does.
fn raw_string(s: &str) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let mut j = 1;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    let hashes = j - 1;
    let terminator = format!("\"{}", "#".repeat(hashes));
    let body_start = j + 1;
    let end = s[body_start..]
        .find(&terminator)
        .map_or(s.len(), |k| body_start + k);
    Some((&s[body_start..end], (end + terminator.len()).min(s.len())))
}

/// The body and total byte length of the normal string literal starting at `s`.
///
/// `\` + newline + indentation is how a query is written across source lines; rustc collapses it
/// to nothing, and it is collapsed to a space here so the query matches as one string.
fn normal_string(s: &str) -> (String, usize) {
    let bytes = s.as_bytes();
    let mut body = String::new();
    let mut j = 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => {
                let mut k = j + 1;
                if bytes.get(k) == Some(&b'\n') {
                    while k < bytes.len() && (bytes[k] as char).is_ascii_whitespace() {
                        k += 1;
                    }
                    body.push(' ');
                    j = k;
                } else {
                    body.push_str(&s[j..(j + 2).min(s.len())]);
                    j += 2;
                }
            }
            b'"' => return (body, j + 1),
            _ => {
                let ch = s[j..].chars().next().unwrap_or('\0');
                body.push(ch);
                j += ch.len_utf8();
            }
        }
    }
    (body, bytes.len())
}

fn squeeze(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/core; the workspace is two levels up.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(&root.join("crates"), &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == "target" || name.starts_with('.') {
                continue;
            }
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every ALLOWLIST entry must carry a reason and still match a real query.
///
/// An exemption is a hole in the guard, so it has to justify itself and it has to stay pointed at
/// something. A stale entry — the query rewritten or the file renamed — is worse than no entry:
/// it reads as a considered exemption while guarding nothing, and it would silently absorb a
/// genuinely new violation that happened to contain the same fragment.
#[test]
fn every_allowlist_entry_is_justified_and_live() {
    let root = workspace_root();
    for (file, fragment, reason) in ALLOWLIST {
        assert!(
            reason.trim().len() >= 20,
            "ALLOWLIST entry {file} / {fragment:?} needs a real reason, got {reason:?}"
        );

        let path = root.join(file);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("ALLOWLIST entry names {file}, which cannot be read: {e}"));

        let matched = sql_literals(&src).into_iter().any(|q| q.contains(fragment));
        assert!(
            matched,
            "ALLOWLIST entry for {file} no longer matches any query there.\n               fragment: {fragment:?}\n               The query was probably rewritten or moved — delete the entry, or update the fragment."
        );
    }
}

/// The guard must actually look inside test files.
///
/// Test paths were skipped wholesale until 2026-08-21, which is why four negative tests using the
/// row-losing shape went unnoticed. If that exemption is ever reinstated, this fails.
#[test]
fn test_paths_are_in_scope() {
    let root = workspace_root();
    let sources = rust_sources(&root);
    let rels: Vec<String> = sources
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert!(
        rels.iter().any(|r| r.contains("/tests/")),
        "the walk must reach integration-test files"
    );
    assert!(
        rels.iter().any(|r| r == SELF_PATH),
        "the walk must reach this file, which the scan then skips by name"
    );
}

/// The guard must reach the probe battery tree specifically (GRD-02).
///
/// `test_paths_are_in_scope` already proves the walk reaches `/tests/` files and this file
/// itself — this is the direct template aimed at `crates/probe/src/` (originally
/// `crates/cli/src/commands/probe/`, created by plan 31-03 and extracted into its own workspace
/// crate by plan 43-01). A future change that accidentally
/// narrows the walk — or an empty-allowlist-style regression like `842c0d2` — fails loudly here
/// rather than passing vacuously because the walk happened to still reach some other directory.
#[test]
fn probe_subcommand_source_is_in_scope() {
    let root = workspace_root();
    let sources = rust_sources(&root);
    let rels: Vec<String> = sources
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert!(
        rels.iter().any(|r| r.starts_with("crates/probe/src/")),
        "the walk must reach crates/probe/src/ — got {rels:?}"
    );
}

// ── the detector's own tests ─────────────────────────────────────────────────────────────────
//
// A guard that silently stops detecting is worse than no guard, so the matcher is pinned against
// the exact shapes measured in production.

#[test]
fn detects_the_live_ch_psc_shape() {
    let q = "SELECT payload.weight_json AS w FROM graph_events \
             WHERE kind = 'NodeAdded' \
               AND payload.weight_json CONTAINS '\"type\":\"Entity\"' \
             LIMIT 5000";
    assert_eq!(
        broken_rule(q),
        Some("rule 1: indexed `kind` paired with an unindexed CONTAINS".to_string())
    );
}

#[test]
fn detects_the_graph_loader_shape() {
    let q = "SELECT payload, timestamp FROM graph_events WHERE kind = 'NodeAdded' LIMIT 65000";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn a_small_existence_check_with_contains_is_still_rule_1() {
    // LIMIT 1 does not save it — the planner can return zero rows for a row that exists, which
    // turns "already emitted?" into "emit again".
    let q = "SELECT id FROM graph_events WHERE kind = 'MotifDetected' \
             AND payload.result_json CONTAINS $hash LIMIT 1";
    assert_eq!(
        broken_rule(q),
        Some("rule 1: indexed `kind` paired with an unindexed CONTAINS".to_string())
    );
}

#[test]
fn count_is_not_a_violation() {
    // `count()` is the form that stays correct under the planner bug.
    let q = "SELECT count() FROM graph_events WHERE kind = 'NodeAdded' \
             AND payload.weight_json CONTAINS '\"type\":\"Entity\"' GROUP ALL";
    assert_eq!(broken_rule(q), None);
}

#[test]
fn the_paged_replacement_is_not_a_violation() {
    let q = "SELECT id, event_id, kind, payload FROM graph_events \
             WHERE payload.weight_json CONTAINS $marker \
             ORDER BY id LIMIT 20000 START 0";
    assert_eq!(broken_rule(q), None);
}

#[test]
fn a_small_limit_without_contains_is_not_a_violation() {
    let q = "SELECT * FROM graph_events WHERE kind = 'NodeAdded' LIMIT 10";
    assert_eq!(broken_rule(q), None);
}

#[test]
fn a_quote_char_literal_does_not_blind_the_rest_of_the_file() {
    // The defect this guard shipped with. `s.trim_matches('"')` appears throughout the workspace;
    // under a naive quote-pairer it inverts the scan, so every query below it disappears and the
    // guard reports a clean file. `property_accumulation.rs` looked exactly like this and hid two
    // violations.
    let src = "let s = s.trim_matches('\"').to_string();\n\
               let q = \"SELECT * FROM graph_events WHERE kind = 'NodeAdded' LIMIT 65000\";";
    let lits = sql_literals(src);
    assert_eq!(lits.len(), 1, "char literal desynced the scan: {lits:?}");
    assert_eq!(
        broken_rule(&lits[0]),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn an_escaped_quote_char_literal_is_consumed_whole() {
    // `'\''` is four bytes with a quote in the middle — stopping at the wrong one desyncs exactly
    // as the unescaped form does.
    let src = "let c = '\\'';\nlet q = \"SELECT * FROM graph_events WHERE kind = 'NodeAdded' LIMIT 65000\";";
    assert_eq!(sql_literals(src).len(), 1);
}

#[test]
fn queries_quoted_in_comments_are_not_extracted() {
    // Comments describe the broken shapes on purpose — the module docs of `scan.rs` are full of
    // them. Extracting prose would make documenting a defect impossible.
    let src = "// SELECT * FROM graph_events WHERE kind = 'NodeAdded' LIMIT 65000\n\
               /* SELECT payload FROM graph_events LIMIT 99000 */\n\
               let n = 1;";
    assert!(sql_literals(src).is_empty(), "{:?}", sql_literals(src));
}

#[test]
fn a_raw_string_query_is_extracted() {
    let src = "let q = r#\"SELECT * FROM graph_events WHERE kind = 'NodeAdded' LIMIT 65000\"#;";
    let lits = sql_literals(src);
    assert_eq!(lits.len(), 1, "{lits:?}");
    assert_eq!(
        broken_rule(&lits[0]),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn a_binding_named_r_next_to_a_string_is_not_a_raw_string() {
    // `.and_then(|mut r| r.take(0).ok())` is the workspace's most common line shape.
    let src = "let x = q.and_then(|mut r| r.take(0).ok());\n\
               let q = \"SELECT * FROM graph_events WHERE kind = 'NodeAdded' LIMIT 65000\";";
    assert_eq!(sql_literals(src).len(), 1);
}

#[test]
fn continuation_escapes_are_collapsed_before_matching() {
    // Queries are written across source lines with `\` continuations; the detector must see them
    // as one string or every multi-line query silently passes.
    let src = "let q = \"SELECT * FROM graph_events \\\n    WHERE kind = 'NodeAdded' \\\n    AND payload.weight_json CONTAINS 'x' LIMIT 5000\";";
    let lits = sql_literals(src);
    assert_eq!(lits.len(), 1, "continuation join failed: {lits:?}");
    assert!(broken_rule(&lits[0]).is_some());
}

// ── per-table fixtures (WATCHED_TABLES widening) ────────────────────────────────────────────
//
// One fixture per table proves the scanner actually flags a known-bad shape there, not just
// that the table name is present in a list somewhere.

#[test]
fn detects_a_bare_limit_on_identity_links() {
    let q = "SELECT * FROM identity_links WHERE status = 'pending_review' LIMIT 500";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn detects_indexed_equality_paired_with_contains_on_identity_links() {
    let q = "SELECT * FROM identity_links WHERE status = 'auto_linked' \
             AND evidence_json CONTAINS 'x' LIMIT 5";
    assert_eq!(
        broken_rule(q),
        Some("rule 1: indexed `status` paired with an unindexed CONTAINS".to_string())
    );
}

#[test]
fn detects_a_bare_limit_on_enrichment_runs() {
    let q = "SELECT * FROM enrichment_runs WHERE source = 'ch_psc' LIMIT 5";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn detects_a_bare_limit_on_oc_quota() {
    let q = "SELECT * FROM oc_quota WHERE year_month = '2026-08' LIMIT 5";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn detects_a_bare_limit_on_land_flip_candidates() {
    let q = "SELECT * FROM land_flip_candidates WHERE status = 'pending_review' LIMIT 500";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn detects_a_bare_limit_on_ppd_transactions() {
    // ppd_transactions' threshold is deliberately 1_000, not 1 (see WATCHED_TABLES) — this must
    // use a magnitude at or above that threshold to trip Rule 2.
    let q = "SELECT * FROM ppd_transactions WHERE district_code = 'W05' LIMIT 50000";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn a_small_limit_on_ppd_transactions_is_not_flagged() {
    // The exact shape at crates/enricher/src/land_flip.rs:611-615 — a legitimate LIMIT 200
    // gate-1 join query, scoped to a single exact address triple. D-16 (2026-08-27, Phase 34)
    // assessed this shape and closed it: 200 PPD transactions against one address is not a
    // reachable state, so it is not row-losing and needs no paged replacement. Proves the
    // deliberately-high 1_000 threshold does what its WATCHED_TABLES comment claims: this shape
    // is left alone, not accidentally caught.
    let q = "SELECT transaction_id, postcode_norm, paon_norm, street_norm, price_gbp, \
             date_of_transfer FROM ppd_transactions \
             WHERE postcode_norm = $pc AND paon_norm = $paon AND street_norm = $street \
             LIMIT 200";
    assert_eq!(broken_rule(q), None);
}

#[test]
fn detects_a_bare_limit_on_analyses() {
    let q = "SELECT * FROM analyses WHERE label = 'x' LIMIT 50000";
    assert_eq!(
        broken_rule(q),
        Some("rule 2: large bare LIMIT with no paging".to_string())
    );
}

#[test]
fn a_unique_key_analyses_lookup_is_not_flagged() {
    // The exact shape at crates/cli/src/registry.rs:102 — db_name carries a UNIQUE index, so at
    // most one row can ever match regardless of the planner. Proves the deliberately-2 (not 1)
    // threshold does what its WATCHED_TABLES comment claims.
    let q = "SELECT * FROM analyses WHERE db_name = $db_name LIMIT 1";
    assert_eq!(broken_rule(q), None);
}

#[test]
fn detects_the_council_registers_planner_bug_shape_if_reintroduced() {
    // Python's analyst_probe.py:2040-2044 issues this predicate as a `count()` aggregate
    // (`SELECT count() AS n FROM graph_events WHERE kind = 'NodeAdded' AND payload.weight_json
    // CONTAINS '"type":"PoliticalActor"' GROUP ALL`). `count()` is DELIBERATELY immune to Rule 1
    // by this guard's own design -- see this file's module doc comment: "count() is unaffected
    // by the planner bug ... it is the thing that stays correct while the row-returning form
    // truncates" -- and `broken_rule` returns `None` unconditionally for any query containing
    // `count(`, before Rule 1 is even evaluated. Plugging Python's literal count()-shaped query
    // into this fixture therefore could never trip Rule 1 (confirmed: `broken_rule` returns
    // `None` for it), so this fixture instead plants the ROW-RETURNING form of the identical
    // `kind`/`CONTAINS` predicate -- the shape a naive client-side-count Rust port would actually
    // be vulnerable to -- and asserts the guard rejects THAT (VRD-09). The ported
    // `council_registers_political_actors.rs` (Plan 33-08) never issues either literal form: it
    // routes through `Attributed`, which itself routes through `scan_events_by_kind` (a bare
    // `kind =` equality, no `CONTAINS` in the same SQL statement -- Rule 1 safe by construction),
    // so no ALLOWLIST entry is needed for the real code. This is a THROWAWAY string fixture, not
    // a claim about what the real probe implements.
    let q = "SELECT id FROM graph_events WHERE kind = 'NodeAdded' AND \
             payload.weight_json CONTAINS '\"type\":\"PoliticalActor\"'";
    assert_eq!(
        broken_rule(q),
        Some("rule 1: indexed `kind` paired with an unindexed CONTAINS".to_string())
    );
}

// ── GRD-08: ordering-bias rule (Rule 3) ──────────────────────────────────────────────────────

#[test]
fn detects_ordering_bias_regardless_of_limit_magnitude() {
    // graph_events' own Rule-2 threshold is 1_000 — LIMIT 10 here is deliberately far below it,
    // proving that widening the table list alone (Rule 2 at any per-table threshold) does NOT
    // catch this class; only the new magnitude-independent Rule 3 does.
    let q = "SELECT * FROM graph_events WHERE kind = 'NodeAdded' ORDER BY timestamp DESC LIMIT 10";
    assert_eq!(
        broken_rule(q),
        Some("rule 3: ordered read with a LIMIT ceiling and no START (biased sample)".to_string())
    );

    // The same shape, scaled down to LIMIT 10, on identity_links (links.rs:52-56) and
    // land_flip_candidates (flips.rs:72-78) — confirming Rule 3 fires on the other watched
    // tables too, not just graph_events.
    let identity_links_q =
        "SELECT * FROM identity_links WHERE status = $status ORDER BY confidence DESC LIMIT 10";
    assert_eq!(
        broken_rule(identity_links_q),
        Some("rule 3: ordered read with a LIMIT ceiling and no START (biased sample)".to_string())
    );

    let land_flip_q = "SELECT * FROM land_flip_candidates WHERE status = $status \
             ORDER BY created_at DESC LIMIT 10";
    assert_eq!(
        broken_rule(land_flip_q),
        Some("rule 3: ordered read with a LIMIT ceiling and no START (biased sample)".to_string())
    );
}

// ── positive fixtures: the paged replacements this milestone ships are not violations ───────

#[test]
fn the_new_identity_links_scan_shape_is_not_a_violation() {
    // The exact shape scan_identity_links_paged (crates/core/src/persistence/scan.rs) produces.
    let q = "SELECT id, source_node_id, target_node_id, resolution_type, confidence, source, \
             status, evidence_json, resolved_at FROM identity_links WHERE status = $status \
             ORDER BY id LIMIT 20000 START 0";
    assert_eq!(broken_rule(q), None);
}

#[test]
fn the_new_land_flip_candidates_scan_shape_is_not_a_violation() {
    // The exact shape scan_land_flip_candidates_paged (crates/core/src/persistence/scan.rs)
    // produces.
    let q = "SELECT * FROM land_flip_candidates WHERE status = $status \
             ORDER BY id LIMIT 20000 START 0";
    assert_eq!(broken_rule(q), None);
}
