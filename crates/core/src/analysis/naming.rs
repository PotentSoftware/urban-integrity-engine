//! Pure, offline naming for per-analysis databases.
//!
//! Scheme: `an_<label>_<wardset_hash8>` where `label` is a slugified analyst
//! string and `wardset_hash8` is the first 8 hex chars of SHA-256 over the
//! sorted, de-duplicated ONS ward codes. Content-addressable: the same ward set
//! always yields the same suffix, so re-provisioning is idempotent/detectable.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Reserved database names that may not be `create` targets.
const RESERVED: &[&str] = &["main", "uk_national"];

/// Slugify to `[a-z0-9_]`: lowercase, collapse any run of non-alphanumeric
/// characters to a single `_`, trim leading/trailing `_`.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_us = true; // leading — suppresses a leading underscore
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Slugify, erroring if the result is empty (label was blank or all punctuation).
pub fn slugify_checked(input: &str) -> Result<String> {
    let s = slugify(input);
    if s.is_empty() {
        bail!("label slugifies to an empty string: {input:?}");
    }
    Ok(s)
}

/// Validate one ONS GSS ward code: one uppercase letter prefix + 8 digits
/// (e.g. `X05999001`). This is the shape guard, not an existence check.
pub fn validate_ward_code(code: &str) -> Result<()> {
    let bytes = code.as_bytes();
    let shape_ok = bytes.len() == 9
        && bytes[0].is_ascii_uppercase()
        && bytes[1..].iter().all(|b| b.is_ascii_digit());
    if !shape_ok {
        bail!("malformed ONS ward code: {code:?} (expected 1 uppercase letter + 8 digits)");
    }
    Ok(())
}

/// First 8 hex chars of SHA-256 over the sorted, de-duplicated ward codes.
/// Order- and duplicate-independent, so the same set always hashes the same.
pub fn wardset_hash(ward_codes: &[String]) -> Result<String> {
    let full = wardset_hash_full(ward_codes)?;
    Ok(full[..8].to_string())
}

/// The full 64-char SHA-256 hex (stored in the registry; the name uses the first 8).
pub fn wardset_hash_full(ward_codes: &[String]) -> Result<String> {
    use std::fmt::Write;
    if ward_codes.is_empty() {
        bail!("ward set is empty");
    }
    let mut sorted: Vec<&str> = ward_codes.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hasher = Sha256::new();
    for code in sorted {
        hasher.update(code.as_bytes());
        hasher.update(b"\n"); // delimiter so ["ab","c"] != ["a","bc"]
    }
    // Format bytes → hex manually to avoid pulling in the `hex` crate.
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        write!(hex, "{b:02x}").expect("writing to String never fails");
    }
    Ok(hex)
}

/// Compose the content-addressable database name `an_<label>_<hash8>`.
pub fn analysis_db_name(label: &str, ward_codes: &[String]) -> Result<String> {
    let slug = slugify_checked(label)?;
    let hash8 = wardset_hash(ward_codes)?;
    let name = format!("an_{slug}_{hash8}");
    if !is_valid_db_name(&name) {
        bail!("computed db name is not a valid SurrealDB identifier: {name}");
    }
    Ok(name)
}

/// A valid SurrealDB db identifier for our scheme: `^[a-z][a-z0-9_]*$`.
pub fn is_valid_db_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether a name is reserved and therefore refused as a `create` target.
pub fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// Per-analysis WAL/checkpoint directory so two analyses cannot corrupt each
/// other's checkpoints (spec §3.1 / §4.4).
pub fn checkpoint_path_for(db_name: &str) -> String {
    format!("data/analyses/{db_name}/checkpoints")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_replaces_non_alnum() {
        assert_eq!(slugify("Black Country 2026"), "black_country_2026");
        assert_eq!(slugify("Foo--Bar!!Baz"), "foo_bar_baz");
        assert_eq!(slugify("  trim me  "), "trim_me");
    }

    #[test]
    fn slugify_rejects_empty() {
        assert!(slugify_checked("").is_err());
        assert!(slugify_checked("!!!").is_err());
    }

    #[test]
    fn wardset_hash_is_order_independent() {
        let a = wardset_hash(&["E05001001".into(), "E05001002".into()]).unwrap();
        let b = wardset_hash(&["E05001002".into(), "E05001001".into()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn wardset_hash_is_dedup_invariant() {
        let a =
            wardset_hash(&["E05001001".into(), "E05001001".into(), "E05001002".into()]).unwrap();
        let b = wardset_hash(&["E05001002".into(), "E05001001".into()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn wardset_hash_rejects_empty_set() {
        assert!(wardset_hash(&[]).is_err());
    }

    #[test]
    fn validate_ward_code_accepts_ons_gss() {
        assert!(validate_ward_code("E05001492").is_ok());
        assert!(validate_ward_code("W05001492").is_ok());
    }

    #[test]
    fn validate_ward_code_rejects_malformed() {
        assert!(validate_ward_code("").is_err());
        assert!(validate_ward_code("12345678").is_err());
        assert!(validate_ward_code("E0500149").is_err());
        assert!(validate_ward_code("E050014922").is_err());
        assert!(validate_ward_code("e05001492").is_err());
    }

    #[test]
    fn analysis_db_name_matches_identifier_regex() {
        let name = analysis_db_name("Black Country 2026", &["E05001001".into()]).unwrap();
        assert!(name.starts_with("an_black_country_2026_"));
        assert!(is_valid_db_name(&name), "got: {name}");
    }

    #[test]
    fn analysis_db_name_is_idempotent() {
        let a = analysis_db_name("BC", &["E05001002".into(), "E05001001".into()]).unwrap();
        let b = analysis_db_name("BC", &["E05001001".into(), "E05001002".into()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn reserved_names_rejected() {
        assert!(is_reserved("main"));
        assert!(is_reserved("uk_national"));
        assert!(!is_reserved("an_foo_deadbeef"));
    }

    #[test]
    fn checkpoint_path_is_per_db() {
        assert_eq!(
            checkpoint_path_for("an_foo_deadbeef"),
            "data/analyses/an_foo_deadbeef/checkpoints"
        );
    }
}
