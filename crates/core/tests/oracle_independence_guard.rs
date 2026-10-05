//! ORC-06's source-level independence guard: proves `crates/core/src/persistence/oracle.rs`
//! contains no executable call into `persistence::scan`, structurally rather than by convention.
//!
//! # Why not a substring ban
//!
//! This project's history records that grep-based plan/test criteria in this project have self-tripped 4x
//! before (Phase 35: a plan repeatedly banning a literal string the plan itself requires). A naive
//! `grep -L "scan::" oracle.rs` is exactly that trap: `oracle.rs`'s own module doc comment
//! legitimately needs to explain why the module avoids `persistence::scan` (see D-02/ORC-06 in
//! the phase 38 design notes), and a substring ban would flag the module for containing that explanation.
//!
//! This test instead models `crates/core/tests/query_shape_guard.rs`'s proven technique: **strip
//! comments and string literals first, then match call syntax** on what remains. [`code_only`]
//! reuses that file's byte-classification loop shape (comment / char-literal / raw-string /
//! normal-string detection) but inverted — instead of extracting string bodies, it accumulates
//! every byte NOT inside a comment, char literal, or string literal, preserving order. The
//! returned text contains only genuine Rust code tokens; a `scan::` mention inside a comment or
//! doc string can never appear in it.
//!
//! # Why this test does not walk the whole workspace
//!
//! Per phase 38 design decision D-06 and phase 38 research Q3, the invariant this guard enforces is "this
//! ONE module never calls scan", not "no module anywhere calls scan" — `query_shape_guard.rs`
//! already owns the workspace-wide invariant for a different property. [`TARGET_FILES`] is a
//! fixed, named path list rather than a directory walk.

use std::path::PathBuf;

/// The file(s) this guard's independence invariant applies to.
const TARGET_FILES: &[&str] = &["crates/core/src/persistence/oracle.rs"];

/// This test file, relative to the workspace root.
///
/// Not used to skip a workspace-wide walk — unlike `query_shape_guard.rs`, this guard only
/// inspects the fixed [`TARGET_FILES`] list, which will never include this file. Declared anyway
/// so the positive/negative fixtures below (which deliberately embed the literal string `scan::`)
/// are documented as living inside the one file this guard's own matcher is never run against,
/// and so a future extension of `TARGET_FILES` to a directory walk has a ready-made self-exemption
/// to reach for.
#[allow(dead_code)]
const SELF_PATH: &str = "crates/core/tests/oracle_independence_guard.rs";

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/core; the workspace is two levels up.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

/// `src` with every comment, char literal, and string-literal BODY removed, preserving the order
/// of everything else. The result contains only real Rust code tokens (identifiers, operators,
/// punctuation) — a `scan::` substring can only ever survive here as call syntax, never as prose.
///
/// Byte-classification loop shape copied from `query_shape_guard.rs`'s `sql_literals` and
/// adapted: inverted to accumulate non-literal, non-comment bytes instead of extracting string
/// bodies.
fn code_only(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        // Line comments.
        if src[i..].starts_with("//") {
            i = src[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            continue;
        }
        // Block comments — nest in Rust.
        if src[i..].starts_with("/*") {
            let mut depth = 1usize;
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
        // Char literals (`'"'`, `'\''`) and lifetimes (`'a`) — never call syntax, and the first
        // is the exact byte that desyncs a naive quote-pairing scan.
        if bytes[i] == b'\'' {
            i += char_literal_len(&src[i..]).unwrap_or(1);
            continue;
        }
        // Raw strings: `r"…"`, `r#"…"#`. Body dropped whole; preceding byte must not be an
        // identifier character, or a binding named `…r` next to a string reads as one.
        if bytes[i] == b'r' && !i.checked_sub(1).is_some_and(|p| is_ident_byte(bytes[p])) {
            if let Some((_, len)) = raw_string(&src[i..]) {
                i += len;
                continue;
            }
        }
        // Normal strings.
        if bytes[i] == b'"' {
            let (_, len) = normal_string(&src[i..]);
            i += len;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte length of the char literal starting at `s`, or `None` if this quote opens a lifetime.
fn char_literal_len(s: &str) -> Option<usize> {
    let rest = &s[1..];
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    let after_first = if first == '\\' {
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

/// The total byte length of the raw string starting at `s`, if one does. Body discarded.
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

/// The body and total byte length of the normal string literal starting at `s`. Body returned for
/// signature symmetry with `query_shape_guard.rs`'s copy but not used here — only `len` matters.
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

/// ORC-06's structural gate: `oracle.rs` must contain zero executable calls into
/// `persistence::scan`.
///
/// Checks both `scan::` (a qualified call site, e.g. `scan::scan_events_by_type(...)`) and
/// `persistence::scan` (an unqualified `use persistence::scan;` followed by bare `scan::` calls
/// — same substring, but checking both catches a `use` statement even if the matcher's `scan::`
/// check alone would already catch the resulting call sites too; belt and braces per the two
/// distinct textual forms D-06 names).
#[test]
fn oracle_module_never_calls_persistence_scan() {
    let root = workspace_root();
    for rel in TARGET_FILES {
        let path = root.join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("TARGET_FILES entry {rel} cannot be read: {e}"));
        let code = code_only(&src);
        let has_scan_call = code.contains("scan::");
        let has_scan_import = code.contains("persistence::scan");
        assert!(
            !has_scan_call && !has_scan_import,
            "{rel} contains an executable call into persistence::scan (ORC-06 violation).\n\n\
             has `scan::` call syntax: {has_scan_call}\n\
             has `persistence::scan` mention: {has_scan_import}\n\n\
             Stripped (comment/string-free) source:\n{code}"
        );
    }
}

/// Non-vacuity proof (positive fixture): the matcher must actually fire on a real `scan::` call
/// site, embedded here as a string so this test's own use of the words does not trip the guard
/// above (this file is not in [`TARGET_FILES`]).
#[test]
fn detects_a_seeded_scan_call_in_code() {
    let src = "use urban_integrity_core::persistence::scan;\n\
               async fn x(db: &Surreal<Any>) -> anyhow::Result<u64> {\n\
                   let rows = scan::scan_events_by_type(db, \"k\", \"m\").await?;\n\
                   Ok(rows.len() as u64)\n\
               }";
    let code = code_only(src);
    assert!(
        code.contains("scan::"),
        "matcher failed to fire on a seeded scan:: call site — the real guard depends on this \
         firing to ever fail: {code:?}"
    );
    assert!(
        code.contains("persistence::scan"),
        "matcher failed to fire on a seeded `persistence::scan` use statement: {code:?}"
    );
}

/// Self-trip-hazard proof (negative fixture): a legitimate doc comment explaining the module
/// avoids `persistence::scan`, plus a real raw-SurrealQL `count()` call, must NOT be flagged. This
/// is the exact kind of prose D-02/D-06 require `oracle.rs`'s own module doc comment to contain.
#[test]
fn does_not_fire_on_a_scan_rs_doc_comment_or_raw_surrealql() {
    let src = "// this module deliberately does not call persistence::scan -- see scan.rs\n\
               // (ORC-06): a defect in one implementation must be visible as a disagreement\n\
               // with the other, never silently shared via scan::scan_latest_entities_by_type.\n\
               async fn count(db: &Surreal<Any>) -> anyhow::Result<u64> {\n\
                   let rows: Vec<serde_json::Value> = db\n\
                       .query(\"SELECT count() FROM graph_events GROUP ALL\")\n\
                       .await?\n\
                       .take(0)?;\n\
                   Ok(rows.first().and_then(|r| r[\"count\"].as_i64()).unwrap_or(0).max(0) as u64)\n\
               }";
    let code = code_only(src);
    assert!(
        !code.contains("scan::") && !code.contains("persistence::scan"),
        "guard self-tripped on legitimate doc-comment prose or raw SurrealQL: {code:?}"
    );
}
