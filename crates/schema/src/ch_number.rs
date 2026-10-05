use tracing::info;

/// A normalised Companies House number with audit metadata.
pub struct NormalisedChNumber {
    /// The normalised registration number (upper-cased, stripped of noise chars).
    pub normalised: String,
    /// The original raw string as supplied by the caller.
    pub original: String,
    /// Ordered list of transformations applied during normalisation (for audit trail, D-17).
    pub transforms_applied: Vec<String>,
    /// True when the number is a bare 6-digit sequence, which is prefix-ambiguous.
    ///
    /// English companies have 8 digits (or 00-prefixed 6 digits); Scottish companies use SC
    /// prefix + 6 digits. A bare 6-digit number cannot be safely zero-padded without
    /// disambiguation evidence — callers must use confidence 0.60 / pending_review (D-16, POL-07).
    pub is_prefix_ambiguous: bool,
}

/// Validate a Companies House number — shared logic across adapters.
///
/// Accepts: pure digits (e.g. "12345678"), or letter-prefix + digits (e.g. "SC123456").
/// Rejects: empty, longer than 10 chars, any non-ASCII-alphanumeric character.
/// Must contain at least 2 digits.
///
/// # Note
/// Copied from `crates/enricher/src/adapters/ch_psc.rs` — the schema crate cannot depend
/// on the enricher crate (would create a dependency cycle).
fn is_valid_ch_number(s: &str) -> bool {
    if s.is_empty() || s.len() > 10 {
        return false;
    }
    let bytes = s.as_bytes();
    let all_safe = bytes.iter().all(|&b| b.is_ascii_alphanumeric());
    if !all_safe {
        return false;
    }
    let digit_count = bytes.iter().filter(|&&b| b.is_ascii_digit()).count();
    digit_count >= 2
}

/// Normalise a raw Companies House registration number string.
///
/// Applies the following transforms in order:
/// 1. Trim leading/trailing whitespace and convert to uppercase.
/// 2. Strip trailing punctuation: `?`, `.`, space.
/// 3. Remove embedded spaces.
/// 4. Detect bare 6-digit ambiguity (POL-07: prohibited from zero-padding without disambiguation).
/// 5. Validate via `is_valid_ch_number`; return `None` if invalid.
///
/// Returns `None` for empty input or strings that fail validation after normalisation.
///
/// # Audit trail
/// `transforms_applied` lists every mutation performed, in insertion order.
/// Callers should persist this in `evidence_json` on any resulting `identity_link` (D-17).
///
/// # Examples
/// ```ignore
/// let r = normalise_ch_number("12345678?").unwrap();
/// assert_eq!(r.normalised, "12345678");
/// assert!(r.transforms_applied.contains(&"strip_trailing_punctuation".to_string()));
/// ```
pub fn normalise_ch_number(raw: &str) -> Option<NormalisedChNumber> {
    let original = raw.to_string();
    let mut s = raw.trim().to_uppercase();
    let mut transforms: Vec<String> = Vec::new();

    // Step 2: strip trailing noise (?, ., space)
    let cleaned = s.trim_end_matches(['?', '.', ' ']);
    if cleaned != s.as_str() {
        transforms.push("strip_trailing_punctuation".to_string());
        s = cleaned.to_string();
    }

    // Step 3: remove embedded spaces
    if s.contains(' ') {
        s = s.replace(' ', "");
        transforms.push("remove_embedded_spaces".to_string());
    }

    // Step 4: detect bare 6-digit prefix ambiguity (D-16, POL-07)
    let is_ambiguous = s.len() == 6 && s.chars().all(|c| c.is_ascii_digit());
    if is_ambiguous {
        transforms.push("bare_six_digit_ambiguous".to_string());
        info!(
            original = %original,
            normalised = %s,
            "EC: 6-digit CH number is ambiguous — will write at 0.60/pending_review (D-16)"
        );
    }

    // Step 5: validate the normalised form
    if !is_valid_ch_number(&s) {
        return None;
    }

    Some(NormalisedChNumber {
        normalised: s,
        original,
        transforms_applied: transforms,
        is_prefix_ambiguous: is_ambiguous,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalise_ch_number_table_driven() {
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("12345678", Some("12345678"), false),   // clean 8-digit
            ("SC123456", Some("SC123456"), false),   // prefixed, not ambiguous
            ("123456", Some("123456"), true),        // bare 6-digit → ambiguous
            ("12345678?", Some("12345678"), false),  // trailing ? stripped
            ("12 345 678", Some("12345678"), false), // embedded spaces removed
            ("12345678.", Some("12345678"), false),  // trailing . stripped
            ("INVALID", None, false),                // fails is_valid_ch_number
            ("", None, false),                       // empty → None
        ];

        for (raw, expected_normalised, expected_ambiguous) in cases {
            let result = normalise_ch_number(raw);
            match expected_normalised {
                Some(exp) => {
                    let r = result.unwrap_or_else(|| panic!("expected Some for '{raw}'"));
                    assert_eq!(r.normalised, *exp, "normalised mismatch for '{raw}'");
                    assert_eq!(
                        r.is_prefix_ambiguous, *expected_ambiguous,
                        "ambiguous flag mismatch for '{raw}'"
                    );
                }
                None => {
                    assert!(result.is_none(), "expected None for '{raw}'");
                }
            }
        }
    }
}
