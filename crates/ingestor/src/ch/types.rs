use serde::Deserialize;

/// Top-level Companies House streaming event envelope.
///
/// Reference: https://developer-specs.company-information.service.gov.uk/streaming-api
#[derive(Deserialize, Debug, Clone)]
pub struct ChEvent {
    pub event: ChEventMeta,
    /// The Companies House company number (e.g. "12345678")
    pub resource_id: String,
    /// Resource type ("company-profile", "officer-appointment", etc.)
    pub resource_kind: String,
    pub data: ChEventData,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ChEventMeta {
    pub timepoint: i64,
    pub published_at: String, // ISO8601
    #[serde(default)]
    pub fields_changed: Vec<String>,
    #[serde(rename = "type")]
    pub event_type: String, // "changed", "deleted"
}

/// Flattened data payload — covers both company-profile and officer-appointment events.
/// All fields are Option<> because resource_kind determines which fields are populated.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ChEventData {
    // Company-profile fields
    pub company_name: Option<String>,
    pub company_number: Option<String>,
    pub company_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_of_cessation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_of_creation: Option<String>,
    pub registered_office_address: Option<ChAddress>,
    // Officer-appointment fields
    pub name: Option<String>,
    pub officer_role: Option<String>,
    pub appointed_on: Option<String>,
    pub resigned_on: Option<String>,
    pub address: Option<ChAddress>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ChAddress {
    pub address_line_1: Option<String>,
    pub locality: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
}
