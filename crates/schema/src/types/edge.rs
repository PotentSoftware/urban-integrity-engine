use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EdgeMeta {
    pub created_at: DateTime<Utc>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DirectsData {
    pub details: String,
    /// Officer role from Companies House ("director", "secretary", etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub officer_role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocatedAtData {
    pub details: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AwardedToData {
    pub details: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct IssuedByData {
    pub details: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RepresentsData {
    pub details: String,
}

// ── OSINT edge data structs (Phase 16) ────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OwnsPropertyData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_paid: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenure: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeclaresInterestData {
    /// LPA code of the council whose register the declaration came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub council: Option<String>,
    /// Register category as published (e.g. "Employment, office, trade").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DisqualifiedFromData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disqualified_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disqualified_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_number: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
pub enum EdgeWeight {
    Directs {
        meta: EdgeMeta,
        data: DirectsData,
    },
    LocatedAt {
        meta: EdgeMeta,
        data: LocatedAtData,
    },
    AwardedTo {
        meta: EdgeMeta,
        data: AwardedToData,
    },
    IssuedBy {
        meta: EdgeMeta,
        data: IssuedByData,
    },
    Represents {
        meta: EdgeMeta,
        data: RepresentsData,
    },
    // NEW: OSINT edge types (Phase 16) — injected by enricher, not ingest pipeline
    Controls {
        meta: EdgeMeta,
    },
    TrusteeOf {
        meta: EdgeMeta,
    },
    OwnsProperty {
        meta: EdgeMeta,
        data: OwnsPropertyData,
    },
    OffshoreLink {
        meta: EdgeMeta,
    },
    DonatesTo {
        meta: EdgeMeta,
    },
    DisqualifiedFrom {
        meta: EdgeMeta,
        data: DisqualifiedFromData,
    },
    /// A councillor's declared interest in a company, from a council register of members'
    /// interests (POL-02).
    ///
    /// Deliberately NOT `Directs`. "Is a director of" is a Companies House fact; "declared an
    /// interest in" is a self-reported disclosure with different evidential weight, and the two
    /// must not be conflated — `enricher::charity_crossover` scans `Directs` edges with no
    /// source-kind filter and reports "Officer X directs contract-holder AND is trustee of
    /// charity", so a declaration arriving as a `Directs` edge would be read as a directorship
    /// inside a conflict-of-interest finding.
    DeclaresInterest {
        meta: EdgeMeta,
        data: DeclaresInterestData,
    },
}
