//! D-03's static-scan half (35-14) -- a durable guard over `crates/probe/src/` (formerly `crates/cli/src/commands/probe/`) AND
//! its `report/` subdirectory, following `probe_tier1_e2e.rs::tier1_probes_issue_no_raw_queries`
//! (`probe_tier1_e2e.rs:387-454`)'s exact shape: `workspace_root()`, `production_portion()` (text
//! before a file's first `#[cfg(test)]` marker), a per-file walk, and a `checked_any` non-vacuity
//! assertion. This is a NEW file rather than growing `probe_tier1_e2e.rs`/`probe_tier2_e2e.rs`
//! further, because this phase's scan must descend into `report/` too -- a tree shape neither
//! existing file's flat `read_dir` walk needs to know about (plan 35-14).
//!
//! This is a static source-text scan -- it opens no DB connection, so it shows no gated/ungated
//! duration delta, matching the precedent file's own note.
//!
//! ## Why this is NOT a blind `.contains("(s)")` substring search
//!
//! A plain `!production.contains("(s)")` -- the literal shape `tier1_probes_issue_no_raw_queries`
//! uses for its own (much less ambiguous) `.query(` ban -- was tried first against this exact tree
//! and DOES false-positive: `Some(s)`, `Ok(s)`, and a turbofish call like
//! `serde_json::from_str::<serde_json::Value>(s)` (`s` a single-character binding, this codebase's
//! own idiom for "the inner str") all contain the literal three characters `(`, `s`, `)` as a
//! substring -- nine such call sites exist in probe production code today (verified live,
//! 2026-08-30), none of them D-03's mechanical-pluralisation defect. The scan below is therefore
//! STRING-LITERAL-AWARE: it only flags `(s)` when it falls inside a double-quoted Rust string
//! literal (where D-03's defect always lives -- either live `format!` prose or a doc comment
//! quoting example prose), never in surrounding code syntax.
//!
//! This exclusion is proven, not assumed -- see
//! `paren_s_detector_catches_the_defect_and_ignores_ordinary_rust_syntax` below, which exercises
//! the exact detection helper this file's scan calls, against small in-test fixtures (never
//! against production code), before the scan is trusted to run over the real tree.
//!
//! D-08's sibling scan (`sort_unstable`/`GRD-07`) is added to this same file further down, with
//! its own module-level rationale for why it too is not a blind substring search.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/cli; the workspace is two levels up.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

/// The production-code portion of a probe/report module's source: everything before its first
/// `#[cfg(test)]` marker. Copied verbatim from `probe_tier1_e2e.rs`'s helper of the same name.
fn production_portion(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(idx) => &src[..idx],
        None => src,
    }
}

/// Recursively collects every `.rs` file under `dir`, depth-first. Unlike `probe_tier1_e2e.rs`'s
/// and `probe_tier2_e2e.rs`'s flat `read_dir` walks, this one must descend into `report/` too
/// (plan 35-14) -- this workspace has no `walkdir` crate dependency to reach for, so this is a
/// small manual `std::fs::read_dir` + `path.is_dir()` recursion instead.
fn collect_rs_files_recursively(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("could not read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_recursively(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

// ── Test 1: no_mechanical_pluralisation_in_probe_or_report_modules (D-03) ───────────────────────

/// Whether `text` contains the literal mechanical-pluralisation marker `(s)` INSIDE a
/// double-quoted Rust string literal -- string-literal-aware rather than a blind
/// `.contains("(s)")`, per this file's module doc comment. `\"` inside a string does not end it;
/// nothing else needs escape-awareness for this scan's purpose -- no raw strings or byte strings
/// appear anywhere in this tree's probe/report production text (verified live, 2026-08-30).
fn contains_paren_s_in_string_literal(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            } else if c == '(' && chars.get(i + 1) == Some(&'s') && chars.get(i + 2) == Some(&')') {
                return true;
            }
        } else if c == '"' {
            in_string = true;
        }
        i += 1;
    }
    false
}

/// Falsifiability proof for `contains_paren_s_in_string_literal`, run against small in-test
/// fixtures -- never against production code (the scan below is what checks production code).
/// Proves both halves of this helper's job: (a) it DOES catch a reintroduced `(s)`-shaped
/// mechanical-pluralisation literal, and (b) it does NOT trip on the `Some(s)`/`Ok(s)`/turbofish
/// call shapes this file's module doc comment names as the reason a blind substring ban is wrong.
#[test]
fn paren_s_detector_catches_the_defect_and_ignores_ordinary_rust_syntax() {
    // (a) A deliberately reintroduced D-03-shaped literal, in a scratch fixture string standing
    // in for a probe's `format!` call -- must be caught.
    let reintroduced_defect = r#"format!("{} link(s) found", n)"#;
    assert!(
        contains_paren_s_in_string_literal(reintroduced_defect),
        "the paren-s detector failed to catch a deliberately reintroduced `(s)`-shaped \
         mechanical-pluralisation literal -- it cannot be trusted as a guard if it cannot fail"
    );

    // (b) The three ordinary-Rust-syntax shapes this module doc comment names as false-positive
    // risks for a blind substring ban -- none of these are inside a string literal, so none
    // should trip the detector.
    let ordinary_code = r#"
        if let Some(s) = v.as_str() { s.to_owned() } else { String::new() }
    "#;
    assert!(
        !contains_paren_s_in_string_literal(ordinary_code),
        "the paren-s detector false-positived on `Some(s)` -- it is not string-literal-aware"
    );
    let ordinary_ok_code = "match parse(x) { Ok(s) => s, Err(e) => return Err(e) }";
    assert!(
        !contains_paren_s_in_string_literal(ordinary_ok_code),
        "the paren-s detector false-positived on `Ok(s)`"
    );
    let ordinary_turbofish_code =
        ".and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())";
    assert!(
        !contains_paren_s_in_string_literal(ordinary_turbofish_code),
        "the paren-s detector false-positived on a turbofish call passing a single-character `s` \
         binding"
    );
}

/// D-03: a static scan asserting no probe or report module's PRODUCTION text (the portion before
/// any `#[cfg(test)]` fixture block) carries the machine-pluralisation defect -- a `(s)` literal
/// appended regardless of count (`"1 link(s)"`), rather than routing through `probe::plural(n,
/// singular, plural_form)` (`crates/probe/src/lib.rs`, formerly `crates/cli/src/commands/probe/mod.rs`). Scoped over the WHOLE
/// `crates/probe/src/` tree, recursively -- both the 22 flat probe modules and the
/// `report/` subdirectory (the writer is itself a presentation surface D-03's ban applies to
/// equally; the research notes' explicit note and the phase 35 design notes).
#[test]
fn no_mechanical_pluralisation_in_probe_or_report_modules() {
    let probe_dir = workspace_root().join("crates/probe/src");
    let mut files = Vec::new();
    collect_rs_files_recursively(&probe_dir, &mut files);
    files.sort();

    let mut checked_any = false;
    let mut saw_flat_file = false;
    let mut saw_report_file = false;
    for path in &files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        let production = production_portion(&src);
        checked_any = true;
        if path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("report")
        {
            saw_report_file = true;
        } else {
            saw_flat_file = true;
        }

        assert!(
            !contains_paren_s_in_string_literal(production),
            "{} carries a `(s)` mechanical-pluralisation literal in a string literal outside \
             #[cfg(test)] -- route through probe::plural(n, singular, plural_form) instead (D-03)",
            path.display()
        );
    }
    assert!(
        checked_any,
        "the scan checked zero files under {} -- the walk is broken",
        probe_dir.display()
    );
    assert!(
        saw_flat_file,
        "the recursive walk never visited a flat probe module (e.g. ward_concentration.rs) -- \
         the walk is broken"
    );
    assert!(
        saw_report_file,
        "the recursive walk never descended into report/ (e.g. report/legend.rs) -- the walk \
         does not actually recurse"
    );
}

// ── Test 2: no_hashmap_iteration_or_sort_unstable_in_probe_or_report_modules (D-08/GRD-07) ──────
//
// ## Why this is NOT a blind `.contains("sort_unstable")` substring search
//
// A plain `!production.contains("sort_unstable")` was tried first against this exact tree and
// DOES false-positive: `detector_readiness.rs` and `osint_confirmed_intel.rs` each carry a
// `// D-08: stable .sort(), never .sort_unstable().`-shaped comment directly above their
// (correct) `.sort()` call -- documentation of the Wave 2 fix, not a violation of it (verified
// live, 2026-08-30). A blind substring ban would trip on these two comments FOREVER, which would
// make the scan permanently red against correct code -- the opposite of a durable guard. The scan
// below therefore drops whole-line `//`/`///`/`//!` comments before checking, so it flags an
// actual `.sort_unstable(...)` call site but not a comment naming the method it replaced.
//
// This exclusion is proven, not assumed -- see
// `sort_unstable_scan_catches_real_calls_and_ignores_comment_mentions` below, against small
// in-test fixtures (never against production code), before the scan is trusted to run over the
// real tree.

/// Drops every whole-line `//`/`///`/`//!` comment from `text`, leaving only code lines --
/// per this section's rationale above, needed so the `sort_unstable` ban does not trip on this
/// codebase's own `// D-08: stable .sort(), never .sort_unstable().` comments
/// (`detector_readiness.rs`, `osint_confirmed_intel.rs`), which legitimately NAME the banned
/// method to document why the line below calls `.sort()` instead of reintroducing it.
fn drop_whole_comment_lines(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Falsifiability proof for the `sort_unstable` ban's comment-aware design, against small
/// in-test fixtures -- never against production code. Proves both halves: (a) an actual
/// `.sort_unstable()` call site is still caught after comment-stripping, and (b) a comment that
/// merely NAMES `.sort_unstable()` (to document why the code below avoids it, this codebase's own
/// established idiom) does not trip the ban.
#[test]
fn sort_unstable_scan_catches_real_calls_and_ignores_comment_mentions() {
    // (a) A deliberately reintroduced real call site -- must be caught.
    let reintroduced_call = "all_missing.sort_unstable();\nall_missing.dedup();";
    let stripped = drop_whole_comment_lines(reintroduced_call);
    assert!(
        stripped.contains("sort_unstable"),
        "the sort_unstable detector failed to catch a deliberately reintroduced \
         `.sort_unstable()` call -- it cannot be trusted as a guard if it cannot fail"
    );

    // (b) The exact comment shape this codebase's own Wave 2 fixes use to document the ban --
    // must NOT be caught, or the scan would be permanently red against correct code.
    let comment_only_mention = "// D-08: stable `.sort()`, never `.sort_unstable()`.\n\
                                 all_missing.sort();\n\
                                 all_missing.dedup();";
    let stripped_comment = drop_whole_comment_lines(comment_only_mention);
    assert!(
        !stripped_comment.contains("sort_unstable"),
        "the sort_unstable detector false-positived on a comment that only NAMES the banned \
         method to explain why the code below avoids it"
    );
}

/// D-08/GRD-07: a static scan asserting no probe or report module's production text calls
/// `.sort_unstable()`/`.sort_unstable_by()`/`.sort_unstable_by_key()` -- GRD-07 requires the
/// report path "uses stable `sort_by`, never `sort_unstable*`" for run-to-run determinism.
/// D-08 names exactly two known (already-fixed, Wave 2 plans 35-02/35-04) sites --
/// `detector_readiness.rs`, `osint_confirmed_intel.rs` -- this scan is what keeps a probe 23 (or
/// a future `report/` section) from reintroducing a third.
///
/// This is the PRIMARY, unambiguous assertion. A full type-aware `HashMap`-display-iteration-order
/// ban (the fuller shape GRD-07/D-08 gesture at -- banning raw `.iter()`/`.values()` display
/// iteration over a value actually typed `HashMap`) is deliberately NOT attempted here: this is a
/// source-TEXT scan, not an AST walk, so it cannot see Rust types. A text-only ban on, say,
/// `".values().collect()"` would either (a) miss any `HashMap` iteration not shaped exactly that
/// way, or (b) false-positive on a `Vec`/`BTreeMap`/`HashSet` iteration that happens to share the
/// same call shape but carries no determinism hazard at all (the phase 35 design notes' orchestrator
/// findings: `contradictions.rs`'s two `HashMap::new()` fixture constructions and one
/// `use std::collections::HashMap` are typed-field uses, not display-iteration hazards --
/// exactly the kind of correct code a shape-only text ban cannot distinguish from a violation).
/// `sort_unstable` has no such ambiguity anywhere in this codebase (D-08, LOCKED) and is
/// therefore the ONLY substring this scan bans -- an honest scope statement, not a silent
/// narrowing of GRD-07's fuller intent.
#[test]
fn no_hashmap_iteration_or_sort_unstable_in_probe_or_report_modules() {
    let probe_dir = workspace_root().join("crates/probe/src");
    let mut files = Vec::new();
    collect_rs_files_recursively(&probe_dir, &mut files);
    files.sort();

    let mut checked_any = false;
    for path in &files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        let production = production_portion(&src);
        let code_only = drop_whole_comment_lines(production);
        checked_any = true;

        assert!(
            !code_only.contains("sort_unstable"),
            "{} calls a sort_unstable* variant outside #[cfg(test)] -- probe and report modules \
             must use stable .sort()/.sort_by()/.sort_by_key() for run-to-run determinism \
             (D-08/GRD-07)",
            path.display()
        );
    }
    assert!(
        checked_any,
        "the scan checked zero files under {} -- the walk is broken",
        probe_dir.display()
    );
}
