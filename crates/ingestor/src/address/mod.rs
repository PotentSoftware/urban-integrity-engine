//! Address normalisation for Location node deduplication.
//!
//! Strategy: postcode + house-number exact match.
//! Canonical key: `LOC:{POSTCODE_NORMALISED}:{HOUSE_NUMBER_NORMALISED}`
//!
//! Covers ~85% of UK addresses. OS Places API (UPRN-based) upgrade path documented in
//! the phase 03 (data ingestion) design notes.

use urban_integrity_schema::types::id::EntityId;

pub mod planning;

/// Produce a canonical `EntityId` for a location from raw address fields.
///
/// Returns `None` if no valid postcode can be extracted — in that case,
/// the caller should skip Location node creation and log at debug level.
pub fn normalise_address(raw_postcode: Option<&str>, raw_street: Option<&str>) -> Option<EntityId> {
    let postcode = normalise_postcode(raw_postcode?)?;
    let house_num = extract_house_number(raw_street.unwrap_or(""));
    Some(EntityId(format!("LOC:{}:{}", postcode, house_num)))
}

/// Normalise a UK postcode: uppercase, strip whitespace, validate basic pattern.
/// Returns None for invalid/empty postcodes.
pub fn normalise_postcode(raw: &str) -> Option<String> {
    let normalised = raw.to_uppercase().replace([' ', '\t', '-'], "");

    if normalised.is_empty() || normalised.len() < 5 || normalised.len() > 8 {
        return None;
    }

    // Must be alphanumeric only
    if !normalised.chars().all(|c| c.is_alphanumeric()) {
        return None;
    }

    // Must end in two letters (UK inward code)
    let bytes = normalised.as_bytes();
    let n = bytes.len();
    if !bytes[n - 1].is_ascii_alphabetic() || !bytes[n - 2].is_ascii_alphabetic() {
        return None;
    }

    Some(normalised)
}

/// Extract the house number/name prefix from a street address string.
///
/// Takes the first whitespace-delimited token. Returns it if it starts with
/// a digit (e.g. "42", "1A"), otherwise returns empty string (named buildings).
pub fn extract_house_number(street: &str) -> String {
    let first_token = street
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_uppercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();

    // Only accept tokens that start with a digit
    if first_token.starts_with(|c: char| c.is_ascii_digit()) {
        first_token
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalise_postcode ---

    #[test]
    fn test_normalise_postcode_strips_space() {
        assert_eq!(normalise_postcode("EC1A 1BB"), Some("EC1A1BB".to_string()));
    }

    #[test]
    fn test_normalise_postcode_uppercases() {
        assert_eq!(normalise_postcode("ec1a1bb"), Some("EC1A1BB".to_string()));
    }

    #[test]
    fn test_normalise_postcode_strips_hyphen() {
        assert_eq!(normalise_postcode("SW1H-0BH"), Some("SW1H0BH".to_string()));
    }

    #[test]
    fn test_normalise_postcode_rejects_empty() {
        assert_eq!(normalise_postcode(""), None);
    }

    #[test]
    fn test_normalise_postcode_rejects_too_short() {
        assert_eq!(normalise_postcode("EC1"), None);
    }

    #[test]
    fn test_normalise_postcode_rejects_special_chars() {
        assert_eq!(normalise_postcode("EC1A!BB"), None);
    }

    // --- extract_house_number ---

    #[test]
    fn test_extract_numeric_house_number() {
        assert_eq!(extract_house_number("42 High Street"), "42".to_string());
    }

    #[test]
    fn test_extract_alphanumeric_house_number() {
        assert_eq!(
            extract_house_number("1A Wellington Place"),
            "1A".to_string()
        );
    }

    #[test]
    fn test_extract_named_building_returns_empty() {
        assert_eq!(
            extract_house_number("Town Hall, High Street"),
            String::new()
        );
    }

    #[test]
    fn test_extract_empty_street_returns_empty() {
        assert_eq!(extract_house_number(""), String::new());
    }

    // --- normalise_address (integration) ---

    #[test]
    fn test_normalise_address_produces_canonical_entity_id() {
        let id = normalise_address(Some("EC1A 1BB"), Some("1 High Street"));
        assert_eq!(id, Some(EntityId("LOC:EC1A1BB:1".to_string())));
    }

    #[test]
    fn test_normalise_address_same_address_same_entity_id() {
        let id1 = normalise_address(Some("EC1A 1BB"), Some("1 High Street"));
        let id2 = normalise_address(Some("ec1a1bb"), Some("1 High Street"));
        assert_eq!(id1, id2, "Same address must produce same EntityId");
    }

    #[test]
    fn test_normalise_address_none_postcode_returns_none() {
        let id = normalise_address(None, Some("42 High Street"));
        assert_eq!(id, None, "Missing postcode should yield None");
    }

    #[test]
    fn test_normalise_address_different_postcodes_produce_different_ids() {
        let id1 = normalise_address(Some("EC1A1BB"), Some("1 High Street"));
        let id2 = normalise_address(Some("SW1H0BH"), Some("1 High Street"));
        assert_ne!(
            id1, id2,
            "Different postcodes must produce different EntityIds"
        );
    }

    #[test]
    fn test_normalise_address_without_house_number_is_postcode_only() {
        let id = normalise_address(Some("LS14AP"), Some("Town Hall"));
        // Named building → house number empty → key is "LOC:LS14AP:"
        assert_eq!(id, Some(EntityId("LOC:LS14AP:".to_string())));
    }
}
