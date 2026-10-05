use serde::{Deserialize, Serialize};

/// Typed company status that carries the date when a company entered a terminal state.
/// Replaces the old `company_status: Option<String>` field.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "type", content = "content")]
pub enum EntityStatus {
    /// Company is trading normally.
    #[default]
    Active,
    /// Company has been dissolved; inner value is the date_of_cessation from Companies House.
    Dissolved(chrono::DateTime<chrono::Utc>),
    /// Company is in liquidation / receivership; inner value is the date the status was set.
    InLiquidation(chrono::DateTime<chrono::Utc>),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EntityData {
    pub name: String,
    /// Companies House company number (e.g. "12345678")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_number: Option<String>,
    /// Typed company status — replaces old company_status: Option<String>.
    /// Old SurrealDB records without a `status` field default to EntityStatus::Active via serde.
    #[serde(default)]
    pub status: EntityStatus,
    /// Date of incorporation from Companies House `date_of_creation`. None when CH has not
    /// yet supplied it. Load-bearing for the detective workflow (AC-DET-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incorporation_date: Option<chrono::NaiveDate>,
}

impl EntityData {
    /// Compatibility shim — maps the new typed enum back to a legacy string for
    /// call sites not yet migrated to match on EntityStatus directly.
    pub fn legacy_status(&self) -> Option<String> {
        match &self.status {
            EntityStatus::Active => Some("active".to_string()),
            EntityStatus::Dissolved(_) => Some("dissolved".to_string()),
            EntityStatus::InLiquidation(_) => Some("liquidation".to_string()),
        }
    }

    /// The date this company entered its terminal state, or `None` while it is `Active`.
    ///
    /// The companion to [`EntityData::legacy_status`], which flattens `EntityStatus` to a bare
    /// discriminant string and discards the date each terminal variant carries. Callers that
    /// need both must read both -- the pair is what the projected `entity` table stores as
    /// `entity_status` + `entity_status_date`.
    pub fn status_date(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        match &self.status {
            EntityStatus::Active => None,
            EntityStatus::Dissolved(at) | EntityStatus::InLiquidation(at) => Some(*at),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OfficerData {
    pub name: String,
    /// Officer role from CH ("director", "secretary", "corporate-director", etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub officer_role: Option<String>,
    /// Date appointed (ISO8601 date string, e.g. "2020-01-15")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appointed_on: Option<String>,
    /// Date resigned (ISO8601 date string)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resigned_on: Option<String>,
    /// CH company number this appointment is at, when the source knows it.
    ///
    /// **This is the officer's company scope, and it lives here rather than in the entity id
    /// on purpose.** Officers are keyed `director:{slug}` so that one human is one node across
    /// every appointment — the invariant Phoenix traversal depends on. That key therefore
    /// carries no company, so anything needing to scope an officer to a company must read it
    /// from here.
    ///
    /// The PSC↔officer join is the caller that needs it: matching a PSC at company A against an
    /// officer of the same name at company B is an exact string match on a name alone, with no
    /// corroborating identifier, and produced 196 false cross-company links on 2026-08-03. See
    /// `build_officer_id_set` in `crates/enricher/src/adapters/ch_psc.rs`.
    ///
    /// `None` for officers whose source carries no company scope — planning-portal directors,
    /// and CH officer nodes written before 2026-08-14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_number: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocationData {
    pub name: String,
    /// Normalised UK postcode (uppercase, no spaces, e.g. "EC1A1BB")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_line_1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<String>,
}

/// Three-state provenance for a planning Contract's applicant attribution (D-02).
///
/// Deliberately conservative default: `AbsentAtSource`, never `Published` — an enum value
/// should never silently claim "the source published this applicant" unless explicitly set
/// that way by `derive_applicant_provenance` (crates/ingestor/src/planning/mapper.rs).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApplicantProvenance {
    /// The portal published an applicant name for this application.
    Published,
    /// The portal did not publish an applicant name (or the value was unrecognised).
    #[default]
    AbsentAtSource,
    /// A fetch for the applicant detail failed (e.g. sidecar timeout, non-200 response).
    FetchFailed,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ContractData {
    pub name: String,
    /// OCDS identifier (e.g. "ocds-b5fd17-{UUID}")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocid: Option<String>,
    /// Contract value in GBP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_gbp: Option<f64>,
    /// OCDS tender/award status ("active", "complete", etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// ISO8601 date the notice was published
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_published: Option<String>,
    /// ONS GSS ward code — populated for planning applications, None for CF contracts.
    /// Enables ward-filtered alert queries without a full graph scan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ward_code: Option<String>,
    /// Contracts Finder tender reference (e.g. "CF-XXXX-YYYY"). None for planning-portal contracts.
    /// D-04: backward-compat via serde(default); old DB rows deserialise as None without panicking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tender_ref: Option<String>,
    /// Planning portal application reference (e.g. "FUL/2024/001234"). None for CF contracts.
    /// D-05: backward-compat via serde(default); old DB rows deserialise as None without panicking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_ref: Option<String>,
    /// Planning portal validated/received date (D-07). None for CF contracts and for planning
    /// rows ingested before the sidecar date extension. Old DB rows deserialise as None without
    /// panicking (backward-compat via serde(default)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_received_date: Option<chrono::NaiveDate>,
    /// Three-state provenance for the applicant attribution on this Contract (D-02). None for
    /// CF contracts and for planning rows ingested before this field existed. Old DB rows
    /// deserialise as None without panicking (backward-compat via serde(default)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicant_provenance: Option<ApplicantProvenance>,
    /// Opaque portal-assigned handle for the application detail page (D-06), used by 24-05's
    /// backfill to re-fetch a specific record. None for CF contracts and pre-phase rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal_detail_handle: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WardData {
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct PoliticalActorData {
    pub name: String,
    /// Political party affiliation (e.g. "Labour Party"). Phase 20 POL-02 extension.
    /// `#[serde(default)]` ensures existing serialized records round-trip without migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<String>,
    /// Council name / LPA code for this political actor. Phase 20 POL-02 extension.
    /// `#[serde(default)]` ensures existing serialized records round-trip without migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub council_name: Option<String>,
}

// ── OSINT node data structs (Phase 16) ────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PscData {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub natures_of_control: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CharityData {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charity_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PropertyData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_paid: Option<f64>,
    /// Companies House registration number of the corporate proprietor.
    ///
    /// Populated by the CCOD adapter (Phase 18, PROP-01). Backward-compatible:
    /// existing DB rows without this field deserialise as None.
    /// Used by the Property Accumulation Anomaly detector to efficiently query
    /// "which entity owns this property" without parsing the deterministic node ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_number: Option<String>,

    /// Canonical country of incorporation of the overseas corporate proprietor.
    ///
    /// Populated by the OCOD adapter (Phase 22, PROP-05) via
    /// `enricher::country_canonical::canonicalise_country`. Backward-compatible:
    /// existing DB rows without this field deserialise as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_incorporated: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OffshoreEntityData {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icij_node_id: Option<String>,
}

/// A CDDA 1986 disqualification order made against a person.
///
/// Modelled as its own node kind rather than an edge into `entity` because the Companies House
/// disqualified-officers stream carries **no company**: a disqualification is a general court
/// order barring someone from directing *any* company, and `struct Disqualification` in
/// `ch_disqualified.rs` holds only the two ban dates and the case identifier. Pointing the
/// `DisqualifiedFrom` edge at a company would assert a company-specific ban the source never
/// states. See the POL-02 directs-relate-rejected investigation notes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DisqualificationData {
    /// CH case identifier for the order (`case_identifier` in the stream payload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_number: Option<String>,
    /// Date the ban begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disqualified_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Date the ban ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disqualified_until: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
pub enum NodeWeight {
    Entity(EntityData),
    Officer(OfficerData),
    Location(LocationData),
    Contract(ContractData),
    Ward(WardData),
    PoliticalActor(PoliticalActorData),
    // NEW: OSINT node types (Phase 16) — injected by enricher, not ingest pipeline
    Psc(PscData),
    Charity(CharityData),
    Property(PropertyData),
    OffshoreEntity(OffshoreEntityData),
    Disqualification(DisqualificationData),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // ── Tests for new OSINT NodeWeight variants (Phase 16) ──────────────────

    #[test]
    fn test_psc_node_weight_roundtrip() {
        let weight = NodeWeight::Psc(PscData {
            name: "Jane Doe".to_string(),
            company_number: Some("12345678".to_string()),
            natures_of_control: Some(vec!["ownership-of-shares-75-to-100-percent".to_string()]),
        });
        let json = serde_json::to_string(&weight).unwrap();
        assert!(json.contains("\"type\":\"Psc\""), "serde tag must be 'Psc'");
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    #[test]
    fn test_offshore_entity_node_weight_roundtrip() {
        let weight = NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: "Acme Offshore Ltd".to_string(),
            jurisdiction: Some("British Virgin Islands".to_string()),
            icij_node_id: Some("12345678901234567890123456789012".to_string()),
        });
        let json = serde_json::to_string(&weight).unwrap();
        assert!(
            json.contains("\"type\":\"OffshoreEntity\""),
            "serde tag must be 'OffshoreEntity'"
        );
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    #[test]
    fn test_charity_node_weight_roundtrip() {
        let weight = NodeWeight::Charity(CharityData {
            name: "Good Causes Foundation".to_string(),
            charity_number: Some("1234567".to_string()),
            registered_name: None,
        });
        let json = serde_json::to_string(&weight).unwrap();
        assert!(
            json.contains("\"type\":\"Charity\""),
            "serde tag must be 'Charity'"
        );
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    #[test]
    fn test_property_node_weight_roundtrip() {
        let weight = NodeWeight::Property(PropertyData {
            title_number: Some("LN1234".to_string()),
            tenure: Some("Freehold".to_string()),
            price_paid: Some(350000.0),
            company_number: None,
            country_incorporated: None,
        });
        let json = serde_json::to_string(&weight).unwrap();
        assert!(
            json.contains("\"type\":\"Property\""),
            "serde tag must be 'Property'"
        );
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    #[test]
    fn test_property_company_number_roundtrip() {
        // Verify company_number is included in serialisation and roundtrips correctly
        let weight = NodeWeight::Property(PropertyData {
            title_number: Some("TGL123456".to_string()),
            tenure: Some("Freehold".to_string()),
            price_paid: Some(1_250_000.0),
            company_number: Some("12345678".to_string()),
            country_incorporated: None,
        });
        let json = serde_json::to_string(&weight).unwrap();
        assert!(
            json.contains("\"company_number\":\"12345678\""),
            "company_number must be serialised when Some"
        );
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    #[test]
    fn test_property_company_number_backward_compat() {
        // Old DB rows without company_number field must deserialise as None
        let json = r#"{"type":"Property","title_number":"LN9999","tenure":"Leasehold","price_paid":200000.0}"#;
        let weight: NodeWeight = serde_json::from_str(json).unwrap();
        match weight {
            NodeWeight::Property(data) => {
                assert_eq!(
                    data.company_number, None,
                    "Old rows without company_number must deserialise as None"
                );
                assert_eq!(data.title_number, Some("LN9999".to_string()));
            }
            other => panic!("Expected Property, got: {:?}", other),
        }
    }

    #[test]
    fn test_entity_status_default_is_active() {
        let s: EntityStatus = Default::default();
        assert_eq!(s, EntityStatus::Active);
    }

    #[test]
    fn test_legacy_status_active() {
        let d = EntityData {
            name: "Apex Build Ltd".into(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        };
        assert_eq!(d.legacy_status(), Some("active".to_string()));
    }

    #[test]
    fn test_legacy_status_dissolved() {
        let d = EntityData {
            name: "Gone Ltd".into(),
            company_number: Some("12345678".into()),
            status: EntityStatus::Dissolved(Utc::now()),
            incorporation_date: None,
        };
        assert_eq!(d.legacy_status(), Some("dissolved".to_string()));
    }

    #[test]
    fn test_legacy_status_liquidation() {
        let d = EntityData {
            name: "Bust Ltd".into(),
            company_number: None,
            status: EntityStatus::InLiquidation(Utc::now()),
            incorporation_date: None,
        };
        assert_eq!(d.legacy_status(), Some("liquidation".to_string()));
    }

    #[test]
    fn test_status_date_active_is_none() {
        let d = EntityData {
            name: "Apex Build Ltd".into(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        };
        assert_eq!(
            d.status_date(),
            None,
            "a trading company has no terminal date"
        );
    }

    /// The date is the whole point of the typed enum -- `legacy_status()` throws it away, so
    /// this asserts the exact instant survives rather than merely that Some(_) comes back.
    #[test]
    fn test_status_date_returns_the_exact_dissolution_instant() {
        let at = chrono::DateTime::parse_from_rfc3339("2021-07-14T09:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let d = EntityData {
            name: "Gone Ltd".into(),
            company_number: Some("12345678".into()),
            status: EntityStatus::Dissolved(at),
            incorporation_date: None,
        };
        assert_eq!(d.status_date(), Some(at));
        assert_eq!(d.legacy_status(), Some("dissolved".to_string()));
    }

    #[test]
    fn test_status_date_returns_the_exact_liquidation_instant() {
        let at = chrono::DateTime::parse_from_rfc3339("2019-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let d = EntityData {
            name: "Bust Ltd".into(),
            company_number: None,
            status: EntityStatus::InLiquidation(at),
            incorporation_date: None,
        };
        assert_eq!(d.status_date(), Some(at));
        assert_eq!(d.legacy_status(), Some("liquidation".to_string()));
    }

    #[test]
    fn test_entity_status_serde_roundtrip_active() {
        let s = EntityStatus::Active;
        let json = serde_json::to_string(&s).unwrap();
        let back: EntityStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EntityStatus::Active);
    }

    #[test]
    fn test_entity_status_serde_roundtrip_dissolved() {
        let dt = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let s = EntityStatus::Dissolved(dt);
        let json = serde_json::to_string(&s).unwrap();
        let back: EntityStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EntityStatus::Dissolved(dt));
    }

    #[test]
    fn test_node_weight_entity_roundtrip() {
        let weight = NodeWeight::Entity(EntityData {
            name: "Test Co".into(),
            company_number: Some("12345678".into()),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        let json = serde_json::to_string(&weight).unwrap();
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    #[test]
    fn test_node_weight_officer_roundtrip() {
        let weight = NodeWeight::Officer(OfficerData {
            name: "Jane Smith".into(),
            officer_role: Some("director".into()),
            appointed_on: Some("2020-01-15".into()),
            resigned_on: None,
            company_number: None,
        });
        let json = serde_json::to_string(&weight).unwrap();
        let back: NodeWeight = serde_json::from_str(&json).unwrap();
        assert_eq!(back, weight);
    }

    // ── Tests for ApplicantProvenance (Phase 24, D-02) ──────────────────────

    #[test]
    fn test_applicant_provenance_default_is_absent_at_source() {
        let p: ApplicantProvenance = Default::default();
        assert_eq!(p, ApplicantProvenance::AbsentAtSource);
    }

    #[test]
    fn test_applicant_provenance_serde_roundtrip_published() {
        let p = ApplicantProvenance::Published;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"published\"");
        let back: ApplicantProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ApplicantProvenance::Published);
    }

    #[test]
    fn test_applicant_provenance_serde_roundtrip_absent_at_source() {
        let p = ApplicantProvenance::AbsentAtSource;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"absent_at_source\"");
        let back: ApplicantProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ApplicantProvenance::AbsentAtSource);
    }

    #[test]
    fn test_applicant_provenance_serde_roundtrip_fetch_failed() {
        let p = ApplicantProvenance::FetchFailed;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"fetch_failed\"");
        let back: ApplicantProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ApplicantProvenance::FetchFailed);
    }

    #[test]
    fn test_contract_data_provenance_backward_compat() {
        // Old DB rows without applicant_provenance/portal_detail_handle must deserialise as None
        // (D-07: the ~39,860 pre-phase rows never carried these fields).
        let json = r#"{"type":"Contract","name":"FUL/2024/001234"}"#;
        let weight: NodeWeight = serde_json::from_str(json).unwrap();
        match weight {
            NodeWeight::Contract(data) => {
                assert_eq!(
                    data.applicant_provenance, None,
                    "Old rows without applicant_provenance must deserialise as None"
                );
                assert_eq!(
                    data.portal_detail_handle, None,
                    "Old rows without portal_detail_handle must deserialise as None"
                );
            }
            other => panic!("Expected Contract, got: {:?}", other),
        }
    }
}
