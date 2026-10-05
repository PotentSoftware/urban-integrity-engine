use serde::Deserialize;

/// Top-level OCDS release package from Contracts Finder.
///
/// Reference: https://www.contractsfinder.service.gov.uk/Published/Notices/OCDS/Search
#[derive(Deserialize, Debug, Clone)]
pub struct OcdsPackage {
    pub releases: Vec<OcdsRelease>,
    pub links: Option<OcdsLinks>,
}

/// Pagination links (OCDS extensions pagination)
#[derive(Deserialize, Debug, Clone)]
pub struct OcdsLinks {
    pub next: Option<String>,
}

/// A single OCDS release (tender or award notice)
#[derive(Deserialize, Debug, Clone)]
pub struct OcdsRelease {
    pub ocid: String,
    pub id: String,
    /// ISO8601 release date
    pub date: Option<String>,
    /// Release tags: ["tender"], ["award"], etc.
    #[serde(default)]
    pub tag: Vec<String>,
    pub tender: Option<OcdsTender>,
    #[serde(default)]
    pub parties: Vec<OcdsParty>,
    pub buyer: Option<OcdsRef>,
    pub awards: Option<Vec<OcdsAward>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OcdsTender {
    pub id: String,
    pub title: Option<String>,
    #[serde(rename = "datePublished")]
    pub date_published: Option<String>,
    pub status: Option<String>,
    pub value: Option<OcdsValue>,
    #[serde(rename = "contractPeriod")]
    pub contract_period: Option<OcdsDateRange>,
}

/// A party (buyer, supplier, tenderer, etc.)
#[derive(Deserialize, Debug, Clone)]
pub struct OcdsParty {
    pub id: String,
    pub name: Option<String>,
    pub identifier: Option<OcdsIdentifier>,
    pub address: Option<OcdsAddress>,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// OCDS identifier — may reference Companies House via scheme="GB-COH"
#[derive(Deserialize, Debug, Clone)]
pub struct OcdsIdentifier {
    /// Identifier scheme: "GB-COH", "GB-SRS", "GB-CFS", etc.
    pub scheme: Option<String>,
    /// Identifier value — company number when scheme = "GB-COH"
    pub id: Option<String>,
    #[serde(rename = "legalName")]
    pub legal_name: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OcdsAddress {
    #[serde(rename = "streetAddress")]
    pub street_address: Option<String>,
    pub locality: Option<String>,
    #[serde(rename = "postalCode")]
    pub postal_code: Option<String>,
    #[serde(rename = "countryName")]
    pub country_name: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OcdsAward {
    pub id: String,
    pub status: Option<String>,
    pub date: Option<String>,
    #[serde(rename = "datePublished")]
    pub date_published: Option<String>,
    pub value: Option<OcdsValue>,
    #[serde(default)]
    pub suppliers: Vec<OcdsRef>,
    #[serde(rename = "contractPeriod")]
    pub contract_period: Option<OcdsDateRange>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OcdsValue {
    pub amount: Option<f64>,
    pub currency: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OcdsDateRange {
    #[serde(rename = "startDate")]
    pub start_date: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
}

/// Minimal reference object (id + name)
#[derive(Deserialize, Debug, Clone)]
pub struct OcdsRef {
    pub id: String,
    pub name: Option<String>,
}
