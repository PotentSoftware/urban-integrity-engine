//! `EntityRef` — a lightweight pointer to a graph entity, embedded wherever a wire type needs to
//! name an entity without carrying its full profile (Analysis page evidence rows today; the
//! Graph/entity picker in a later phase).
//!
//! Like every type in this crate, no field is `#[serde(default)]` — an omitted key is a
//! deserialization error, not a silently-defaulted empty string.

use serde::{Deserialize, Serialize};

/// A named pointer to a graph entity. `kind` is a plain `String` (not a tagged enum) — this type
/// carries no domain invariant to protect (unlike `VerdictView`, which mirrors an
/// invariant-bearing type), so a plain string is the correct, minimal shape per this crate's "view
/// types carry no domain invariant" rule.
///
/// Two producers fill it and they are now spelled to agree:
///
/// * `crates/workbench`'s `node_kind_name` — the node class of a graph node (`"Entity"`,
///   `"Officer"`, `"Location"`, `"Contract"`, `"Ward"`, …). This is what the entity search, the
///   profile header and every profile-row pivot carry.
/// * `urban_integrity_schema::types::verdict::RecordKind::name` — the pivot-target class of a
///   probe's `RecordRef` (`"Entity"`, `"CompanyNumber"`, `"IdentityLink"`, `"Contract"`,
///   `"Location"`). This is what the Analysis page's evidence refs carry.
///
/// The original comment here claimed only the second list, which was already untrue when written:
/// `LabelIndex` has put node-class names in this field since 44-06. D-06 (plan 45-11) fixed the
/// overlap rather than the comment alone — the two node classes both vocabularies name are
/// spelled identically, so `"Location"` means one thing on this field regardless of which producer
/// wrote it. `"CompanyNumber"` and `"IdentityLink"` remain probe-only; the `NodeWeight`-only
/// classes (`"Officer"`, `"Ward"`, `"Psc"`, …) remain graph-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityRef {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EntityRef {
        EntityRef {
            id: "director:jane-smith".to_string(),
            name: "Jane Smith".to_string(),
            kind: "Entity".to_string(),
        }
    }

    /// `EntityRef` round-trips through `serde_json::to_string`/`from_str`.
    #[test]
    fn entity_ref_round_trips() {
        let entity_ref = sample();
        let json = serde_json::to_string(&entity_ref).expect("EntityRef must serialize");
        let back: EntityRef = serde_json::from_str(&json).expect("EntityRef must deserialize");
        assert_eq!(back, entity_ref);
    }

    /// `kind` is a plain non-`Option` field with no `#[serde(default)]` — an omitted key must
    /// fail to deserialize, per the crate's blanket "no default" rule.
    ///
    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`'s "Proving a Test Can Fail"): this test was
    /// run against a version of `EntityRef` with `#[serde(default)]` added to `kind` — the
    /// assertion failed (deserialization succeeded with `kind: String::new()` instead of
    /// erroring) — before the attribute was removed to the version committed here.
    #[test]
    fn missing_kind_key_fails_to_deserialize() {
        let missing_kind = r#"{"id":"director:jane-smith","name":"Jane Smith"}"#;
        assert!(
            serde_json::from_str::<EntityRef>(missing_kind).is_err(),
            "an omitted `kind` key must be a deserialization error, not a silent default"
        );
    }
}
