//! Country-of-incorporation canonicalisation (Phase 22, PROP-05).
//!
//! Maps the many raw spellings of a jurisdiction (e.g. `"BVI"`,
//! `"British Virgin Islands"`, `"BRITISH VIRGIN ISLANDS"`) to a single canonical
//! token. The OCOD adapter (Plan 03) MUST canonicalise BEFORE any jurisdiction
//! filtering — filtering raw strings would silently drop records with
//! non-standard country spellings.
//!
//! **PROP-05 Pitfall-2 guard:** an unknown / non-standard country string is
//! returned UNCHANGED (trimmed) — it is NEVER dropped. The caller is responsible
//! for logging a `WARN` on a passthrough so unmapped jurisdictions stay visible
//! and the table can be extended.
//!
//! The reference table (`data/country_incorporation_canonical.json`) is compiled
//! in via `include_str!`, so canonicalisation is a pure, CWD-independent function
//! that works identically in unit tests, the CLI, and the server.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Committed raw→canonical reference table: `{ canonical: [variants…] }` plus a
/// leading `_meta` object. Compiled into the binary at build time.
const CANONICAL_TABLE_JSON: &str =
    include_str!("../../../data/country_incorporation_canonical.json");

/// Normalise a country string for lookup: trim, uppercase, and collapse all
/// internal whitespace runs to a single space. This is the matching key — it is
/// NOT what gets returned (the canonical display name or the trimmed original is).
fn normalise_key(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

/// Build the reverse lookup map (normalised variant → canonical display name)
/// once, from the committed JSON table. The canonical name itself is also
/// registered as a variant so an already-canonical input round-trips.
fn lookup_map() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| {
        let parsed: serde_json::Value = serde_json::from_str(CANONICAL_TABLE_JSON)
            .expect("country_incorporation_canonical.json must be valid JSON (committed data)");
        let obj = parsed
            .as_object()
            .expect("country_incorporation_canonical.json root must be a JSON object");

        let mut map = HashMap::new();
        for (canonical, variants) in obj {
            // Skip the metadata block.
            if canonical == "_meta" {
                continue;
            }
            // The canonical name maps to itself.
            map.insert(normalise_key(canonical), canonical.clone());
            if let Some(arr) = variants.as_array() {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        map.insert(normalise_key(s), canonical.clone());
                    }
                }
            }
        }
        map
    })
}

/// Canonicalise a country-of-incorporation string to its single canonical token.
///
/// - Known variants (case- and whitespace-insensitive) map to one canonical name:
///   `canonicalise_country("BVI") == canonicalise_country("British Virgin Islands")`.
/// - An unknown / non-standard string is returned **trimmed but otherwise
///   unchanged** — it is NEVER dropped (PROP-05 Pitfall-2 guard). Callers should
///   log a `WARN` when the result equals the trimmed input and was not a known
///   canonical name, so unmapped jurisdictions remain visible.
/// - A blank / whitespace-only string returns `""`.
pub fn canonicalise_country(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    lookup_map()
        .get(&normalise_key(trimmed))
        .cloned()
        .unwrap_or_else(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_map_to_one_canonical_token() {
        let canonical = canonicalise_country("British Virgin Islands");
        assert_eq!(canonical, "British Virgin Islands");
        // Abbreviation, full name, and shout-case all converge.
        assert_eq!(canonicalise_country("BVI"), canonical);
        assert_eq!(canonicalise_country("BRITISH VIRGIN ISLANDS"), canonical);
        assert_eq!(
            canonicalise_country("  british   virgin  islands "),
            canonical
        );
        assert_eq!(canonicalise_country("Br. Virgin Is."), canonical);
    }

    #[test]
    fn other_jurisdictions_canonicalise() {
        assert_eq!(canonicalise_country("uae"), "United Arab Emirates");
        assert_eq!(canonicalise_country("USA"), "United States");
        assert_eq!(canonicalise_country("isle of man"), "Isle of Man");
        assert_eq!(canonicalise_country("CAYMAN"), "Cayman Islands");
    }

    #[test]
    fn unknown_string_passes_through_trimmed_never_dropped() {
        // PROP-05 Pitfall-2: unmapped jurisdictions are preserved, not dropped.
        assert_eq!(
            canonicalise_country("Some Unmapped Jurisdiction 9000"),
            "Some Unmapped Jurisdiction 9000"
        );
        assert_eq!(
            canonicalise_country("  Some Unmapped Jurisdiction 9000  "),
            "Some Unmapped Jurisdiction 9000"
        );
    }

    #[test]
    fn blank_passes_through() {
        assert_eq!(canonicalise_country(""), "");
        assert_eq!(canonicalise_country("   "), "");
    }
}
