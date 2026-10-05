//! Canonicalising a node id read out of a SurrealDB row.
//!
//! A related-record field may deserialise as a plain string, as a quoted record id
//! (`entity:⟨GB-COH:01234567⟩`), or as a `{"tb":..,"id":..}` object. Two detectors papered over
//! that with bidirectional `contains`, which turns an identifier comparison into a prefix match:
//! `director:john` matched `director:john-smith`, attributing one person's status and contract
//! totals to another, and `.find()` returned an arbitrary row when several qualified.
//!
//! Canonicalise once at map-build time, then compare exactly — the exact-match-only policy
//! (2026-06-13), which removed every other fuzzy identifier join in this codebase.

/// The bare node id inside `v`, with record-id wrapping removed.
///
/// Returns `None` for null, empty, or unreadable values — a caller must not treat those as a
/// match against anything.
pub fn canonical_entity_id(v: &serde_json::Value) -> Option<String> {
    if let Some(id) = v.as_object().and_then(|o| o.get("id")) {
        return canonical_entity_id(id);
    }

    let raw = match v.as_str() {
        Some(s) => s.to_string(),
        None => v.to_string(),
    };
    let raw = raw.trim().trim_matches('"').trim();

    // A bare id contains colons of its own (`GB-COH:01234567`), so only strip a leading
    // `table:` for tables this project actually projects — never split on the first colon
    // unconditionally.
    let bare = match raw.split_once(':') {
        Some(("entity" | "contract" | "location" | "officer" | "graph_events", rest)) => rest,
        _ => raw,
    };
    let bare = bare.trim_start_matches('⟨').trim_end_matches('⟩').trim();

    if bare.is_empty() || bare == "null" {
        None
    } else {
        Some(bare.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_id_is_returned_unchanged() {
        assert_eq!(
            canonical_entity_id(&serde_json::json!("GB-COH:01234567")).as_deref(),
            Some("GB-COH:01234567")
        );
    }

    #[test]
    fn a_record_id_wrapping_is_stripped() {
        assert_eq!(
            canonical_entity_id(&serde_json::json!("entity:⟨GB-COH:01234567⟩")).as_deref(),
            Some("GB-COH:01234567")
        );
    }

    #[test]
    fn a_record_object_is_stripped() {
        assert_eq!(
            canonical_entity_id(&serde_json::json!({"tb": "entity", "id": "GB-COH:01234567"}))
                .as_deref(),
            Some("GB-COH:01234567")
        );
    }

    #[test]
    fn a_director_slug_keeps_its_prefix_and_is_not_prefix_matched() {
        // The defect this module exists to remove: `contains` made these equal.
        let short = canonical_entity_id(&serde_json::json!("director:john")).unwrap();
        let long = canonical_entity_id(&serde_json::json!("director:john-smith")).unwrap();
        assert_eq!(short, "director:john", "director is not a projected table");
        assert_ne!(short, long);
        assert!(long.starts_with(&short), "the substring hazard is real");
    }

    #[test]
    fn null_and_empty_yield_none() {
        assert_eq!(canonical_entity_id(&serde_json::json!(null)), None);
        assert_eq!(canonical_entity_id(&serde_json::json!("")), None);
        assert_eq!(canonical_entity_id(&serde_json::json!("entity:⟨⟩")), None);
    }
}
