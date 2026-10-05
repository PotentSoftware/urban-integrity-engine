//! Compile-time glossary parser — `docs/abbreviations.md` -> a queryable Rust table.
//!
//! This is the "one source, two renderers" foundation for NAV-04: Plan 04's inline `<abbr>`
//! component and Plan 08's `/help/glossary` page both consume [`sections()`], [`lookup()`] and
//! [`term_count()`] from here, so the abbreviation set can never drift between the two surfaces.
//!
//! The embedded file (`include_str!`) is parsed once, lazily, behind a [`std::sync::OnceLock`] —
//! a plain, host-testable function with no build-script wiring, per the phase 43 design notes'
//! "Claude's Discretion" note that a `build.rs` is equally acceptable but unnecessary here.
//!
//! # Scope (D-25)
//!
//! `docs/abbreviations.md` has 12 top-level `##` sections. Only the first 10 are genuine
//! `| **ABBR** | Meaning | Notes |` glossary tables; parsing STOPS (excluding) at the heading
//! `Requirement category codes` — everything from that point on, including the following
//! `Land-Flip detector: ...` section, is a different table shape (or free prose) and is never
//! parsed. The nested `### Detector motifs` subsection under `## Statistics & detection` is
//! folded into that section's own row list, not treated as an 11th top-level section.
//!
//! # Failure mode (T-43-03-01)
//!
//! The parser never panics on a malformed row. Only lines matching the exact 3-column glossary
//! shape are accumulated; everything else (prose, blank lines, section intros) is silently
//! skipped. A future malformed edit to `docs/abbreviations.md` degrades to "row missing", not a
//! build/boot crash.

use std::sync::OnceLock;

/// One row of a glossary table: `| **ABBR** | Meaning | Notes |`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryRow {
    pub abbr: &'static str,
    pub meaning: &'static str,
    pub notes: &'static str,
}

/// One `##` (or, for "Statistics & detection", `##` + nested `###`) genuine glossary section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossarySection {
    pub heading: &'static str,
    pub rows: Vec<GlossaryRow>,
}

const ABBREVIATIONS_MD: &str = include_str!("../../../docs/abbreviations.md");

/// The heading at which parsing stops (excluded). Everything from here to EOF is a different
/// table shape or free prose (D-25).
const SCOPE_STOP_HEADING: &str = "Requirement category codes";

static SECTIONS: OnceLock<Vec<GlossarySection>> = OnceLock::new();

/// Returns the parsed, genuine glossary sections of `docs/abbreviations.md`, in file order.
/// Parsed once and cached for the process lifetime.
// Only this plan's own tests call `sections()` today — Plan 04's `<abbr>` component and Plan
// 08's `/help/glossary` page are the real consumers and land in later plans of this phase.
#[allow(dead_code)]
pub fn sections() -> &'static [GlossarySection] {
    SECTIONS.get_or_init(|| parse(ABBREVIATIONS_MD)).as_slice()
}

/// Exact-case lookup of an abbreviation token across every parsed section. Glossary
/// abbreviations are always upper-case in the source file, so this is intentionally
/// case-sensitive: `lookup("ch")` returns `None`, `lookup("CH")` returns `Some`.
// Real consumer is Plan 04's `<abbr>` component (not yet built in this plan).
#[allow(dead_code)]
pub fn lookup(token: &str) -> Option<&'static GlossaryRow> {
    sections()
        .iter()
        .flat_map(|section| section.rows.iter())
        .find(|row| row.abbr == token)
}

/// Total row count across every parsed section — the single source of truth for the Home
/// Help card's term count (Plan 06 calls this, never a hardcoded literal, per D-27).
// Real consumer is Plan 06's Home Help card (not yet built in this plan).
#[allow(dead_code)]
pub fn term_count() -> usize {
    sections().iter().map(|section| section.rows.len()).sum()
}

/// A glossary row: `| **ABBR** | Meaning | Notes |` (3-column) or `| **ABBR** | Meaning |`
/// (2-column — `docs/abbreviations.md` mixes both table shapes across its 10 genuine sections;
/// a 2-column row parses with `notes` as an empty string). `abbr` must be a bare alphanumeric
/// token — node-key prefixes and compound tokens containing `:`, `-`, `_`, `` ` `` or spaces
/// (e.g. `GB-COH:`, `P_c`, `T-Digest`) are a different, deliberately out-of-scope shape and are
/// skipped, not errors (see the module-level failure-mode note).
fn parse_row(line: &'static str) -> Option<GlossaryRow> {
    let line = line.trim_end();
    let content = line.strip_prefix("| ")?.strip_suffix(" |")?;
    let fields: Vec<&str> = content.split(" | ").collect();
    if fields.len() < 2 || fields.len() > 3 {
        return None;
    }
    let abbr = fields[0].strip_prefix("**")?.strip_suffix("**")?;
    if abbr.is_empty() || !abbr.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let meaning = fields[1];
    if meaning.is_empty() {
        return None;
    }
    let notes = fields.get(2).copied().unwrap_or("");
    Some(GlossaryRow {
        abbr,
        meaning,
        notes,
    })
}

/// Parses the embedded glossary markdown into its genuine sections, per the D-25 scope rule
/// documented on this module. Never panics: non-matching lines are skipped, not errors.
fn parse(source: &'static str) -> Vec<GlossarySection> {
    let mut out: Vec<GlossarySection> = Vec::new();
    let mut current: Option<GlossarySection> = None;

    for line in source.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let heading = heading.trim();
            if heading == SCOPE_STOP_HEADING {
                // Flush whatever section was in progress, then stop accumulating entirely —
                // every heading from here to EOF is out of scope (D-25).
                if let Some(section) = current.take() {
                    out.push(section);
                }
                break;
            }
            if let Some(section) = current.take() {
                out.push(section);
            }
            current = Some(GlossarySection {
                heading,
                rows: Vec::new(),
            });
            continue;
        }

        if let Some(subheading) = line.strip_prefix("### ") {
            // Nested subsections (e.g. "Detector motifs" under "Statistics & detection") fold
            // their rows into the enclosing top-level section — never a new top-level section.
            let _ = subheading;
            continue;
        }

        if let Some(section) = current.as_mut() {
            if let Some(row) = parse_row(line) {
                section.rows.push(row);
            }
        }
    }

    if let Some(section) = current.take() {
        out.push(section);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exactly_ten_genuine_sections() {
        assert_eq!(
            sections().len(),
            10,
            "expected exactly the 10 genuine glossary sections before the D-25 scope-stop"
        );
    }

    #[test]
    fn contains_ch_row_with_companies_house_meaning() {
        let row = lookup("CH").expect("CH must be present in the real glossary");
        assert!(
            row.meaning.contains("Companies House"),
            "CH row meaning was: {}",
            row.meaning
        );
    }

    #[test]
    fn does_not_contain_requirement_category_codes_tokens() {
        // PRB ("Probe battery") is a v7.0 requirement-category-code family, defined only in the
        // "Requirement category codes" section this parser must stop before (D-25).
        assert!(
            lookup("PRB").is_none(),
            "PRB must not be parsed — it belongs to the excluded Requirement category codes section"
        );
    }

    #[test]
    fn lookup_is_case_sensitive_on_the_abbreviation() {
        assert!(lookup("ch").is_none(), "lowercase 'ch' must not match");
        assert!(lookup("CH").is_some(), "uppercase 'CH' must match");
    }

    #[test]
    fn term_count_matches_the_real_files_genuine_row_count() {
        let expected: usize = sections().iter().map(|s| s.rows.len()).sum();
        assert_eq!(term_count(), expected);
        // Concrete spot-check: the real file must have parsed a non-trivial number of rows.
        assert!(
            term_count() > 50,
            "term_count() was suspiciously low ({}) — the parser may not be matching real rows",
            term_count()
        );
    }
}
