use anyhow::ensure;
use chrono::{DateTime, Utc};
use serde::de::Error as SerdeDeError;
use serde::{Deserialize, Deserializer, Serialize};

/// Minimum score for a link to be automatically promoted to ConfirmedLink.
pub const AUTO_LINK_THRESHOLD: f64 = 0.85;

/// Minimum score for a link to be recorded as a candidate (pending_review).
pub const CANDIDATE_THRESHOLD: f64 = 0.70;

/// Minimum score for a sub-candidate link (0.50–0.69). Below this → discard.
/// Read-only: surfaced only on entity --deep. NOT actionable via links confirm/reject (D-13).
pub const SUB_CANDIDATE_THRESHOLD: f64 = 0.50;

/// A validated confidence score in the range [0.0, 1.0].
///
/// Constructed only through `ConfidenceScore::new()`, which enforces the range constraint
/// (T-16-04). The field is private — not even `pub(crate)` — so nothing inside this crate
/// can build one by struct literal either. `Deserialize` is hand-written (not derived) so
/// that deserializing a spoofed out-of-range float also routes through `new()` and fails;
/// a bare `#[derive(Deserialize)]` on a tuple struct reads the raw `f64` straight into the
/// field, skipping the range check entirely (T-16-04).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConfidenceScore(f64);

impl ConfidenceScore {
    /// Construct a ConfidenceScore, returning Err if value is outside [0.0, 1.0].
    pub fn new(value: f64) -> anyhow::Result<Self> {
        ensure!(
            (0.0..=1.0).contains(&value),
            "ConfidenceScore value {} is outside the valid range [0.0, 1.0]",
            value
        );
        Ok(Self(value))
    }

    /// Return the underlying f64 score.
    pub fn value(&self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ConfidenceScore {
    /// Deserializes a raw `f64` and routes it through `new()`, so an out-of-range value
    /// (e.g. a spoofed `2.5`) is rejected as a serde error rather than silently accepted
    /// (T-16-04).
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        ConfidenceScore::new(value).map_err(SerdeDeError::custom)
    }
}

/// Status of an identity link in SurrealDB.
///
/// Variant names serialise as snake_case strings that match the SurrealDB
/// `identity_links.status` field values used in confirm/reject queries.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    AutoLinked,
    PendingReview,
    Candidate,
    /// 0.50–0.69 — read-only display on entity --deep only (D-13, D-14).
    SubCandidate,
    Confirmed,
    Rejected,
}

/// Returns the plain snake_case string for `status` without JSON quoting.
///
/// SurrealDB bind parameters bind Rust strings as SurrealDB strings directly.
/// Using `serde_json::to_string(status)` would add an extra layer of JSON quotes,
/// storing `'"auto_linked"'` instead of `'auto_linked'`.
pub fn link_status_str(status: &LinkStatus) -> &'static str {
    match status {
        LinkStatus::AutoLinked => "auto_linked",
        LinkStatus::PendingReview => "pending_review",
        LinkStatus::Candidate => "candidate",
        LinkStatus::SubCandidate => "sub_candidate",
        LinkStatus::Confirmed => "confirmed",
        LinkStatus::Rejected => "rejected",
    }
}

/// A persisted identity link record mirroring the SurrealDB `identity_links` table schema.
///
/// `evidence_json` is always `String` (never `serde_json::Value`) to avoid the
/// 2000x SurrealDB nested-object query regression (ENR-06).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IdentityLinkRecord {
    pub id: String,
    pub source_node_id: String,
    pub target_node_id: String,
    pub resolution_type: String,
    pub confidence: f64,
    pub source: String,
    pub resolved_at: DateTime<Utc>,
    /// Serialised JSON string — never serde_json::Value (ENR-06 compliance).
    pub evidence_json: String,
    pub status: LinkStatus,
}

/// A link whose confidence is at or above `AUTO_LINK_THRESHOLD` (0.85).
///
/// `try_new` is the only construction path — `score` and `record` are private, so
/// `ConfirmedLink { score, record }` cannot be written anywhere outside this module,
/// including from inside this crate. That closes the T-16-05 bypass: previously both
/// fields were `pub`, so a sub-threshold link could be built directly, skipping the
/// threshold check in `try_new` entirely. Use `score()` / `record()` / `into_record()`
/// to read the fields back out.
///
/// # T-16-05, enforced by the compiler
///
/// The struct-literal bypass does not compile. A score of 0.10 is far below
/// `AUTO_LINK_THRESHOLD` (0.85), so `try_new` would refuse it — the literal skips that check:
///
/// ```compile_fail
/// use urban_integrity_enricher::confidence::{
///     ConfidenceScore, ConfirmedLink, IdentityLinkRecord, LinkStatus,
/// };
///
/// let record = IdentityLinkRecord {
///     id: "test:001".to_string(),
///     source_node_id: "entity:test-co".to_string(),
///     target_node_id: "psc:test-psc".to_string(),
///     resolution_type: "name_match".to_string(),
///     confidence: 0.10,
///     source: "doctest".to_string(),
///     resolved_at: chrono::Utc::now(),
///     evidence_json: "{}".to_string(),
///     status: LinkStatus::Candidate,
/// };
/// let score = ConfidenceScore::new(0.10).unwrap();
///
/// // `score` and `record` are private: this is a compile error, not a runtime rejection.
/// let bypass = ConfirmedLink { score, record };
/// ```
///
/// A `compile_fail` doctest passes if the snippet fails to compile for *any* reason, so on its
/// own it is weak evidence — a typo or a bad import would satisfy it just as well as the privacy
/// error it is meant to demonstrate. This companion is identical except that it builds through
/// `try_new`, and it must COMPILE. Together they attribute the failure above to field privacy
/// specifically, rather than to anything incidental in the setup:
///
/// ```
/// use urban_integrity_enricher::confidence::{
///     ConfidenceScore, ConfirmedLink, IdentityLinkRecord, LinkStatus,
/// };
///
/// let record = IdentityLinkRecord {
///     id: "test:001".to_string(),
///     source_node_id: "entity:test-co".to_string(),
///     target_node_id: "psc:test-psc".to_string(),
///     resolution_type: "name_match".to_string(),
///     confidence: 0.10,
///     source: "doctest".to_string(),
///     resolved_at: chrono::Utc::now(),
///     evidence_json: "{}".to_string(),
///     status: LinkStatus::Candidate,
/// };
/// let score = ConfidenceScore::new(0.10).unwrap();
///
/// // Same values, sanctioned path: the threshold gate rejects them at runtime.
/// assert!(ConfirmedLink::try_new(score, record).is_none());
/// ```
pub struct ConfirmedLink {
    score: ConfidenceScore,
    record: IdentityLinkRecord,
}

impl ConfirmedLink {
    /// Returns `None` if `score.value()` is below `AUTO_LINK_THRESHOLD`.
    pub fn try_new(score: ConfidenceScore, record: IdentityLinkRecord) -> Option<Self> {
        if score.value() < AUTO_LINK_THRESHOLD {
            return None;
        }
        Some(Self { score, record })
    }

    /// The link's validated confidence score (always `>= AUTO_LINK_THRESHOLD`).
    pub fn score(&self) -> ConfidenceScore {
        self.score
    }

    /// Borrow the underlying identity link record.
    pub fn record(&self) -> &IdentityLinkRecord {
        &self.record
    }

    /// Consume the link, returning the underlying identity link record.
    pub fn into_record(self) -> IdentityLinkRecord {
        self.record
    }
}

/// A link whose confidence is at or above `CANDIDATE_THRESHOLD` (0.70) but below
/// `AUTO_LINK_THRESHOLD`. These are queued for analyst review.
///
/// `try_new` is the only construction path — see [`ConfirmedLink`]'s doc comment for why
/// the fields are private (same T-16-05-shaped bypass, closed the same way).
pub struct CandidateLink {
    score: ConfidenceScore,
    record: IdentityLinkRecord,
}

impl CandidateLink {
    /// Returns `None` if `score.value()` is below `CANDIDATE_THRESHOLD`.
    pub fn try_new(score: ConfidenceScore, record: IdentityLinkRecord) -> Option<Self> {
        if score.value() < CANDIDATE_THRESHOLD {
            return None;
        }
        Some(Self { score, record })
    }

    /// The link's validated confidence score (always `>= CANDIDATE_THRESHOLD`).
    pub fn score(&self) -> ConfidenceScore {
        self.score
    }

    /// Borrow the underlying identity link record.
    pub fn record(&self) -> &IdentityLinkRecord {
        &self.record
    }

    /// Consume the link, returning the underlying identity link record.
    pub fn into_record(self) -> IdentityLinkRecord {
        self.record
    }
}

/// A link whose confidence is at or above `SUB_CANDIDATE_THRESHOLD` (0.50) but below
/// `CANDIDATE_THRESHOLD`. These are read-only — surfaced on `entity --deep` only (D-13).
/// NOT actionable via `links confirm/reject` (D-14).
///
/// `try_new` is the only construction path — see [`ConfirmedLink`]'s doc comment for why
/// the fields are private (same T-16-05-shaped bypass, closed the same way).
pub struct SubCandidateLink {
    score: ConfidenceScore,
    record: IdentityLinkRecord,
}

impl SubCandidateLink {
    /// The link's validated confidence score (always `>= SUB_CANDIDATE_THRESHOLD`).
    pub fn score(&self) -> ConfidenceScore {
        self.score
    }

    /// Borrow the underlying identity link record.
    pub fn record(&self) -> &IdentityLinkRecord {
        &self.record
    }

    /// Consume the link, returning the underlying identity link record.
    pub fn into_record(self) -> IdentityLinkRecord {
        self.record
    }

    /// Returns `None` if `score.value()` is below `SUB_CANDIDATE_THRESHOLD`.
    ///
    /// Sub-candidate links are read-only display artefacts. The `None` return on
    /// low scores means adapters can call this without a prior threshold check.
    pub fn try_new(score: ConfidenceScore, record: IdentityLinkRecord) -> Option<Self> {
        if score.value() < SUB_CANDIDATE_THRESHOLD {
            return None;
        }
        Some(Self { score, record })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_record() -> IdentityLinkRecord {
        IdentityLinkRecord {
            id: "identity_links:test".to_string(),
            source_node_id: "director:test-person".to_string(),
            target_node_id: "psc:12345".to_string(),
            resolution_type: "ch_psc".to_string(),
            confidence: 0.9,
            source: "ch_psc_adapter".to_string(),
            resolved_at: chrono::Utc::now(),
            evidence_json: "{}".to_string(),
            status: LinkStatus::PendingReview,
        }
    }

    // ConfidenceScore boundary tests

    #[test]
    fn test_confidence_tier_model_out_of_range_high() {
        assert!(ConfidenceScore::new(1.1).is_err());
    }

    #[test]
    fn test_confidence_tier_model_out_of_range_low() {
        assert!(ConfidenceScore::new(-0.01).is_err());
    }

    #[test]
    fn test_confidence_tier_model_valid() {
        let score = ConfidenceScore::new(0.5).unwrap();
        assert!((score.value() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_confidence_tier_model_zero_boundary() {
        assert!(ConfidenceScore::new(0.0).is_ok());
    }

    #[test]
    fn test_confidence_tier_model_one_boundary() {
        assert!(ConfidenceScore::new(1.0).is_ok());
    }

    // ConfirmedLink boundary tests

    #[test]
    fn test_confirmed_link_boundary_below_threshold() {
        // 0.84 is below AUTO_LINK_THRESHOLD (0.85) — must return None
        let score = ConfidenceScore::new(0.84).unwrap();
        assert!(ConfirmedLink::try_new(score, dummy_record()).is_none());
    }

    #[test]
    fn test_confirmed_link_boundary_at_threshold() {
        // 0.85 is exactly at AUTO_LINK_THRESHOLD — must return Some
        let score = ConfidenceScore::new(0.85).unwrap();
        assert!(ConfirmedLink::try_new(score, dummy_record()).is_some());
    }

    #[test]
    fn test_confirmed_link_boundary_above_threshold() {
        let score = ConfidenceScore::new(1.0).unwrap();
        assert!(ConfirmedLink::try_new(score, dummy_record()).is_some());
    }

    // CandidateLink boundary tests

    #[test]
    fn test_candidate_link_boundary_below_threshold() {
        // 0.69 is below CANDIDATE_THRESHOLD (0.70) — must return None
        let score = ConfidenceScore::new(0.69).unwrap();
        assert!(CandidateLink::try_new(score, dummy_record()).is_none());
    }

    #[test]
    fn test_candidate_link_boundary_at_threshold() {
        // 0.70 is exactly at CANDIDATE_THRESHOLD — must return Some
        let score = ConfidenceScore::new(0.70).unwrap();
        assert!(CandidateLink::try_new(score, dummy_record()).is_some());
    }

    // LinkStatus serialization tests

    #[test]
    fn test_link_status_serialization_auto_linked() {
        let json = serde_json::to_string(&LinkStatus::AutoLinked).unwrap();
        assert_eq!(json, "\"auto_linked\"");
    }

    #[test]
    fn test_link_status_serialization_pending_review() {
        let json = serde_json::to_string(&LinkStatus::PendingReview).unwrap();
        assert_eq!(json, "\"pending_review\"");
    }

    #[test]
    fn test_link_status_serialization_confirmed() {
        let json = serde_json::to_string(&LinkStatus::Confirmed).unwrap();
        assert_eq!(json, "\"confirmed\"");
    }

    #[test]
    fn test_link_status_serialization_rejected() {
        let json = serde_json::to_string(&LinkStatus::Rejected).unwrap();
        assert_eq!(json, "\"rejected\"");
    }

    #[test]
    fn test_link_status_serialization_candidate() {
        let json = serde_json::to_string(&LinkStatus::Candidate).unwrap();
        assert_eq!(json, "\"candidate\"");
    }

    // ── link_status_str regression tests (double-quote SurrealDB bug) ──────────

    #[test]
    fn test_link_status_str_all_variants() {
        // Verify every variant returns the expected plain snake_case string.
        assert_eq!(link_status_str(&LinkStatus::AutoLinked), "auto_linked");
        assert_eq!(
            link_status_str(&LinkStatus::PendingReview),
            "pending_review"
        );
        assert_eq!(link_status_str(&LinkStatus::Candidate), "candidate");
        assert_eq!(link_status_str(&LinkStatus::SubCandidate), "sub_candidate");
        assert_eq!(link_status_str(&LinkStatus::Confirmed), "confirmed");
        assert_eq!(link_status_str(&LinkStatus::Rejected), "rejected");
    }

    #[test]
    fn test_link_status_str_differs_from_json_serialization() {
        // Regression: serde_json::to_string(&status) produces '"auto_linked"' (a JSON
        // string literal including surrounding quotes).  When bound directly to SurrealDB
        // that would store the value with embedded quotes, e.g. '"auto_linked"' instead
        // of 'auto_linked'.  link_status_str must equal the JSON-unquoted value.
        for status in [
            LinkStatus::AutoLinked,
            LinkStatus::PendingReview,
            LinkStatus::Candidate,
            LinkStatus::SubCandidate,
            LinkStatus::Confirmed,
            LinkStatus::Rejected,
        ] {
            let bare = link_status_str(&status);
            let json = serde_json::to_string(&status).unwrap(); // e.g. "\"auto_linked\""
                                                                // The JSON string wraps the bare value in quotes.
            assert_eq!(
                format!("\"{}\"", bare),
                json,
                "link_status_str('{bare}') must be the unquoted form of serde_json output '{json}'"
            );
            // Explicitly: bare string must not start/end with a quote character.
            assert!(
                !bare.starts_with('"'),
                "link_status_str must not be JSON-quoted: got {bare:?}"
            );
        }
    }

    // ── SubCandidate tier tests (Phase 19, D-12) ─────────────────────────────────

    #[test]
    fn test_link_status_str_sub_candidate_returns_plain_string() {
        // T-19-01-01: link_status_str must return plain snake_case, never JSON-quoted
        let bare = link_status_str(&LinkStatus::SubCandidate);
        assert_eq!(
            bare, "sub_candidate",
            "link_status_str(&SubCandidate) must return 'sub_candidate' (no JSON quotes)"
        );
    }

    #[test]
    fn test_link_status_sub_candidate_serde_json_has_quotes() {
        // Verifies the two-representation difference the existing regression test checks:
        // serde produces '"sub_candidate"' (with JSON string quotes).
        let json = serde_json::to_string(&LinkStatus::SubCandidate).unwrap();
        assert_eq!(
            json, "\"sub_candidate\"",
            "serde must produce '\"sub_candidate\"' (JSON-quoted)"
        );
    }

    #[test]
    fn test_sub_candidate_threshold_boundary_at_050() {
        // SUB_CANDIDATE_THRESHOLD = 0.50 is inclusive — score of exactly 0.50 is valid
        let score = ConfidenceScore::new(0.50).unwrap();
        let record = dummy_record();
        assert!(
            SubCandidateLink::try_new(score, record).is_some(),
            "score 0.50 must produce Some (boundary is inclusive)"
        );
    }

    #[test]
    fn test_sub_candidate_below_threshold_returns_none() {
        // Score below 0.50 must return None — adapter discards as noise (D-12)
        let score = ConfidenceScore::new(0.499).unwrap();
        let record = dummy_record();
        assert!(
            SubCandidateLink::try_new(score, record).is_none(),
            "score 0.499 must return None (below SUB_CANDIDATE_THRESHOLD)"
        );
    }
}
