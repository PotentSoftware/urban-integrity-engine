use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MotifType {
    PhoenixLoop,
    AddressCluster,
    NepotismBridge,
    #[serde(rename = "property_accumulation_anomaly")]
    PropertyAccumulationAnomaly,
    #[serde(rename = "disqualified_director_evasion")]
    DisqualifiedDirectorEvasion,
    #[serde(rename = "offshore_shadow")]
    OffshoreShadow,
    #[serde(rename = "charity_commercial_crossover")]
    CharityCommercialCrossover,
    #[serde(rename = "donation_contract_correlation")]
    DonationContractCorrelation,
}

/// Whether a `MotifType` ever has more than one named participant.
///
/// Only two of the eight detector types are genuinely single-subject:
/// `PropertyAccumulationAnomaly` (`entity_id` alone) and
/// `DonationContractCorrelation` (`donor_id` alone — `party_name`/`lpa_code` are plain
/// labels written alongside it, not resolvable entity ids). No connecting path is ever
/// drawable for these, by design, not because current data happens to be sparse.
///
/// Every other variant — including the three original motifs and the three post-adapter
/// detectors that name a pair (`officer_id`+`disq_node_id`,
/// `entity_id`+`icij_node_id`, `officer_id`+`charity_node_id`) — is `Structural`: the
/// generic induced-subgraph path renderer applies, and participant count is data-driven.
///
/// This is deliberately two variants, not a per-detector taxonomy: a third or fourth kind
/// would need a rendering-relevant reason to exist, and none of the eight detectors gives
/// one. See the phase 46 plan 03 notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotifKind {
    /// Exactly one named participant; no connecting path can ever be drawn.
    SingleSubject,
    /// Two or more named participants; the induced-subgraph path renderer applies.
    Structural,
}

impl MotifType {
    /// Every `MotifType` variant, in declaration order.
    pub const ALL: [MotifType; 8] = [
        MotifType::PhoenixLoop,
        MotifType::AddressCluster,
        MotifType::NepotismBridge,
        MotifType::PropertyAccumulationAnomaly,
        MotifType::DisqualifiedDirectorEvasion,
        MotifType::OffshoreShadow,
        MotifType::CharityCommercialCrossover,
        MotifType::DonationContractCorrelation,
    ];

    /// Whether this motif ever has more than one named participant.
    ///
    /// Exhaustive by design — no `_ =>` arm. A ninth detector added later must add its own
    /// arm here or the workspace fails to compile (D-04, Phase 50 D-06 precedent).
    pub fn motif_kind(&self) -> MotifKind {
        match self {
            MotifType::PropertyAccumulationAnomaly | MotifType::DonationContractCorrelation => {
                MotifKind::SingleSubject
            }
            MotifType::PhoenixLoop
            | MotifType::AddressCluster
            | MotifType::NepotismBridge
            | MotifType::DisqualifiedDirectorEvasion
            | MotifType::OffshoreShadow
            | MotifType::CharityCommercialCrossover => MotifKind::Structural,
        }
    }

    /// A distinct, non-empty, human-readable name for this motif.
    pub fn display_name(&self) -> &'static str {
        match self {
            MotifType::PhoenixLoop => "Phoenix",
            MotifType::AddressCluster => "Address Cluster",
            MotifType::NepotismBridge => "Nepotism Bridge",
            MotifType::PropertyAccumulationAnomaly => "Property Accumulation",
            MotifType::DisqualifiedDirectorEvasion => "Disqualified Director Evasion",
            MotifType::OffshoreShadow => "Offshore Shadow",
            MotifType::CharityCommercialCrossover => "Charity/Commercial Crossover",
            MotifType::DonationContractCorrelation => "Donation-Contract Correlation",
        }
    }

    /// The exact wire string this variant serialises to — the `#[serde(rename)]` value for
    /// the five new variants, or the un-renamed variant name for the original three.
    /// For callers that need the string without going through `serde_json::to_string`
    /// (e.g. building a facet-count key or a CLI `--motif` filter value).
    pub fn wire_str(&self) -> &'static str {
        match self {
            MotifType::PhoenixLoop => "PhoenixLoop",
            MotifType::AddressCluster => "AddressCluster",
            MotifType::NepotismBridge => "NepotismBridge",
            MotifType::PropertyAccumulationAnomaly => "property_accumulation_anomaly",
            MotifType::DisqualifiedDirectorEvasion => "disqualified_director_evasion",
            MotifType::OffshoreShadow => "offshore_shadow",
            MotifType::CharityCommercialCrossover => "charity_commercial_crossover",
            MotifType::DonationContractCorrelation => "donation_contract_correlation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotifResult {
    pub motif_type: MotifType,
    pub unique_hash: u64,
    pub participants: Vec<String>, // EntityId strings
    pub risk_score: f64,
    pub evidence_snapshot: String, // JSON blob of the evidence
    pub detected_at_event: u64,
    pub version: u32,
    pub last_seen_at: DateTime<Utc>,
    pub score_breakdown: ScoreBreakdown,
    /// Set when an operator retracts this alert; `None` means live.
    ///
    /// Detection upserts what it matches and never withdraws what it stops matching, so an
    /// alert invalidated by a data correction outlives its own correction and is
    /// indistinguishable from a live finding except by `last_seen_at`, which no consumer
    /// reads (planning open item 23). Marking is preferred to deleting because a retracted
    /// false positive is itself evidence about detector quality.
    ///
    /// `serde(default)` is load-bearing: every alert already persisted predates these
    /// fields, and dropping those rows on parse would be a worse failure than the one this
    /// fixes. Note that if the motif starts matching again, the upsert overwrites the
    /// result and the withdrawal is cleared — which is correct, since it is then a live
    /// finding again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub s_phoenix: f64,
    pub s_proximity: f64,
    pub s_clustering: f64,
    pub p_base: f64,
    pub m_env: f64,
    pub p_final: f64,
    pub imd_rank: Option<u32>,
    pub ward_code: Option<String>,
}

/// Compute a stable hash for a motif based on its participants.
/// The participants are sorted before hashing so the order doesn't matter.
pub fn motif_hash(participants: &[&str]) -> u64 {
    let mut sorted_participants = participants.to_vec();
    sorted_participants.sort_unstable();

    let mut combined_bytes = Vec::new();
    for p in sorted_participants {
        combined_bytes.extend_from_slice(p.as_bytes());
        combined_bytes.push(0); // Separator
    }

    xxh3_64(&combined_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_motif_hash_is_stable_and_order_independent() {
        let p1 = vec!["GB-COH:1234", "OFF:1234-SMITH", "OCDS:9999"];
        let p2 = vec!["OCDS:9999", "GB-COH:1234", "OFF:1234-SMITH"];

        let hash1 = motif_hash(&p1);
        let hash2 = motif_hash(&p2);

        assert_eq!(hash1, hash2, "Hashes must be identical regardless of order");
    }

    #[test]
    fn every_motif_type_has_a_motif_kind_and_a_display_name() {
        use std::collections::HashSet;

        let mut names: HashSet<&'static str> = HashSet::new();
        let mut single_subject_count = 0;
        let mut structural_count = 0;

        for motif in MotifType::ALL {
            let name = motif.display_name();
            assert!(
                !name.is_empty(),
                "display_name must be non-empty for {motif:?}"
            );
            assert!(
                names.insert(name),
                "display_name {name:?} is not distinct (duplicate for {motif:?})"
            );

            match motif.motif_kind() {
                MotifKind::SingleSubject => single_subject_count += 1,
                MotifKind::Structural => structural_count += 1,
            }
        }

        assert_eq!(names.len(), 8, "expected 8 distinct display names");
        assert_eq!(
            single_subject_count, 2,
            "expected exactly 2 SingleSubject motifs (PropertyAccumulationAnomaly, DonationContractCorrelation)"
        );
        assert_eq!(structural_count, 6, "expected exactly 6 Structural motifs");

        assert_eq!(
            MotifType::PropertyAccumulationAnomaly.motif_kind(),
            MotifKind::SingleSubject
        );
        assert_eq!(
            MotifType::DonationContractCorrelation.motif_kind(),
            MotifKind::SingleSubject
        );
        assert_eq!(MotifType::PhoenixLoop.motif_kind(), MotifKind::Structural);
        assert_eq!(
            MotifType::AddressCluster.motif_kind(),
            MotifKind::Structural
        );
        assert_eq!(
            MotifType::NepotismBridge.motif_kind(),
            MotifKind::Structural
        );
        assert_eq!(
            MotifType::DisqualifiedDirectorEvasion.motif_kind(),
            MotifKind::Structural
        );
        assert_eq!(
            MotifType::OffshoreShadow.motif_kind(),
            MotifKind::Structural
        );
        assert_eq!(
            MotifType::CharityCommercialCrossover.motif_kind(),
            MotifKind::Structural
        );
    }

    #[test]
    fn motif_type_wire_strings_match_live_corpus() {
        assert_eq!(
            serde_json::to_string(&MotifType::PropertyAccumulationAnomaly).unwrap(),
            "\"property_accumulation_anomaly\""
        );
        assert_eq!(
            serde_json::to_string(&MotifType::DisqualifiedDirectorEvasion).unwrap(),
            "\"disqualified_director_evasion\""
        );
        assert_eq!(
            serde_json::to_string(&MotifType::OffshoreShadow).unwrap(),
            "\"offshore_shadow\""
        );
        assert_eq!(
            serde_json::to_string(&MotifType::CharityCommercialCrossover).unwrap(),
            "\"charity_commercial_crossover\""
        );
        assert_eq!(
            serde_json::to_string(&MotifType::DonationContractCorrelation).unwrap(),
            "\"donation_contract_correlation\""
        );
        // Unchanged: the 3 existing variants get no #[serde(rename)].
        assert_eq!(
            serde_json::to_string(&MotifType::PhoenixLoop).unwrap(),
            "\"PhoenixLoop\""
        );
    }
}
