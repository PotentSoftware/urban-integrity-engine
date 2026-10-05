/// Live integration tests against real UK government data feeds.
///
/// These tests make real HTTP requests to:
///   - Contracts Finder OCDS API (public, no auth)
///   - Companies House REST API (requires URBAN__INGEST__CH_API_KEY)
///
/// **Run live tests with:**
/// ```
/// URBAN_LIVE_TESTS=1 cargo test --test live -- --nocapture
/// URBAN_LIVE_TESTS=1 URBAN__INGEST__CH_API_KEY=your-key cargo test --test live -- --nocapture
/// ```
///
/// Without `URBAN_LIVE_TESTS=1`, all tests in this file are skipped.
/// This prevents CI from making external network calls.
use std::time::Duration;

use reqwest::Client;
use urban_integrity_core::{pipeline::EventPayload, process_event};
use urban_integrity_ingestor::cf::{mapper::map_release, types::OcdsPackage};
use urban_integrity_ingestor::ch::{mapper::map_ch_event, types::ChEvent};

const CF_BASE_URL: &str =
    "https://www.contractsfinder.service.gov.uk/Published/Notices/OCDS/Search";

const CH_REST_BASE: &str = "https://api.company-information.service.gov.uk";

/// Skip the test if URBAN_LIVE_TESTS is not set.
macro_rules! live_only {
    () => {
        if std::env::var("URBAN_LIVE_TESTS").unwrap_or_default() != "1" {
            eprintln!("⏭  Skipping live test — set URBAN_LIVE_TESTS=1 to run");
            return;
        }
    };
}

fn http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("urban-integrity-live-tests/0.1")
        .build()
        .expect("reqwest client")
}

// ────────────────────────────────────────────────────────────────────────────
// Contracts Finder — no auth required
// ────────────────────────────────────────────────────────────────────────────

/// Fetch the latest 5 releases from Contracts Finder and assert the response
/// is a valid OCDS package with at least one release.
#[tokio::test]
async fn live_cf_api_returns_valid_ocds_package() {
    live_only!();

    let client = http_client();
    let resp = client
        .get(format!("{}?limit=5", CF_BASE_URL))
        .send()
        .await
        .expect("CF request failed");

    assert_eq!(
        resp.status(),
        200,
        "CF API returned non-200: {}",
        resp.status()
    );

    let package: OcdsPackage = resp
        .json()
        .await
        .expect("CF response is not valid OcdsPackage JSON");
    assert!(
        !package.releases.is_empty(),
        "CF API returned 0 releases — unexpected"
    );
    eprintln!("✓ CF: received {} releases", package.releases.len());
}

/// Fetch releases and assert that every release has a non-empty ocid and id.
#[tokio::test]
async fn live_cf_releases_have_required_ocds_fields() {
    live_only!();

    let client = http_client();
    let package: OcdsPackage = client
        .get(format!("{}?limit=10", CF_BASE_URL))
        .send()
        .await
        .expect("CF request")
        .json()
        .await
        .expect("CF JSON");

    for release in &package.releases {
        assert!(
            !release.ocid.is_empty(),
            "Release has empty ocid: {:?}",
            release.id
        );
        assert!(!release.id.is_empty(), "Release has empty id");
    }
    eprintln!(
        "✓ CF: {} releases all have required fields",
        package.releases.len()
    );
}

/// Fetch releases and run the mapper over each one. Assert no mapper call panics
/// and that at least some mutations are produced for real data.
#[tokio::test]
async fn live_cf_mapper_produces_graph_mutations_from_real_data() {
    live_only!();

    let client = http_client();
    let package: OcdsPackage = client
        .get(format!("{}?limit=20", CF_BASE_URL))
        .send()
        .await
        .expect("CF request")
        .json()
        .await
        .expect("CF JSON");

    let mut total_mutations = 0usize;
    let mut releases_with_mutations = 0usize;

    for release in &package.releases {
        let mutations = map_release(release);
        total_mutations += mutations.len();
        if !mutations.is_empty() {
            releases_with_mutations += 1;
        }
    }

    eprintln!(
        "✓ CF mapper: {} releases → {} graph mutations ({} with mutations)",
        package.releases.len(),
        total_mutations,
        releases_with_mutations
    );

    assert!(
        total_mutations > 0,
        "mapper produced 0 mutations from {} real CF releases — something is wrong",
        package.releases.len()
    );
}

/// Fetch award-tagged releases specifically and assert buyer/supplier nodes
/// are mapped from real award data.
#[tokio::test]
async fn live_cf_award_releases_produce_contract_and_party_nodes() {
    live_only!();

    let client = http_client();
    // Fetch with tag filter — CF doesn't have a direct tag query param but awards
    // are the most common type. Fetch a larger batch to ensure we get some.
    let package: OcdsPackage = client
        .get(format!("{}?limit=50", CF_BASE_URL))
        .send()
        .await
        .expect("CF request")
        .json()
        .await
        .expect("CF JSON");

    let award_releases: Vec<_> = package
        .releases
        .iter()
        .filter(|r| r.tag.iter().any(|t| t == "award"))
        .collect();

    eprintln!(
        "✓ CF: {} award releases in batch of {}",
        award_releases.len(),
        package.releases.len()
    );

    if award_releases.is_empty() {
        eprintln!("⚠  No award releases in this batch — skipping node assertion");
        return;
    }

    let mut found_contract = false;
    let mut found_party = false;

    for release in &award_releases {
        let ingest_events = map_release(release);
        for event in &ingest_events {
            if let EventPayload::NodeUpsert { weight, .. } = &event.payload {
                use urban_integrity_schema::types::node::NodeWeight;
                match weight {
                    NodeWeight::Contract(_) => found_contract = true,
                    NodeWeight::Entity(_) => found_party = true,
                    _ => {}
                }
            }
        }
    }

    assert!(
        found_contract,
        "No Contract node produced from {} award releases",
        award_releases.len()
    );
    assert!(
        found_party,
        "No Entity (buyer/supplier) node produced from award releases"
    );
    eprintln!("✓ CF award releases → Contract + Entity nodes confirmed");
}

/// Assert that pagination links are present and that following `next` returns
/// a second page of different releases.
#[tokio::test]
async fn live_cf_pagination_next_link_works() {
    live_only!();

    let client = http_client();
    let page1: OcdsPackage = client
        .get(format!("{}?limit=5", CF_BASE_URL))
        .send()
        .await
        .expect("page 1 request")
        .json()
        .await
        .expect("page 1 JSON");

    let next_url = match page1.links.and_then(|l| l.next) {
        Some(u) => u,
        None => {
            eprintln!("⚠  No pagination link returned — CF may be returning full dataset");
            return;
        }
    };

    eprintln!(
        "✓ CF: page 1 has {} releases, following next link",
        page1.releases.len()
    );

    let page2: OcdsPackage = client
        .get(&next_url)
        .send()
        .await
        .expect("page 2 request")
        .json()
        .await
        .expect("page 2 JSON");

    eprintln!("✓ CF: page 2 has {} releases", page2.releases.len());

    // Page 2 must have different ocids from page 1
    let p1_ids: std::collections::HashSet<&str> =
        page1.releases.iter().map(|r| r.ocid.as_str()).collect();
    let p2_ids: std::collections::HashSet<&str> =
        page2.releases.iter().map(|r| r.ocid.as_str()).collect();
    let overlap = p1_ids.intersection(&p2_ids).count();

    assert_eq!(
        overlap, 0,
        "Page 2 contains {} duplicate ocids from page 1 — pagination broken",
        overlap
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Companies House REST API — requires URBAN__INGEST__CH_API_KEY
// ────────────────────────────────────────────────────────────────────────────

fn ch_api_key() -> Option<String> {
    // Companies House issues TYPED keys: the streaming key (URBAN__INGEST__CH_API_KEY)
    // 401s against the REST API, which needs the separate REST key
    // (URBAN__ENRICHER__CH__API_KEY). These live tests call the REST API, so prefer the
    // REST key and fall back to the streaming key only for back-compat.
    for var in ["URBAN__ENRICHER__CH__API_KEY", "URBAN__INGEST__CH_API_KEY"] {
        let key = std::env::var(var).unwrap_or_default();
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

/// Fetch a well-known UK company (BBC) from the CH REST API and assert basic
/// company profile fields are present and parse correctly.
#[tokio::test]
async fn live_ch_company_profile_fetch_and_parse() {
    live_only!();
    let key = match ch_api_key() {
        Some(k) => k,
        None => {
            eprintln!("⏭  CH REST test skipped — URBAN__INGEST__CH_API_KEY not set");
            return;
        }
    };

    let client = http_client();
    // BBC's Companies House number is a well-known stable fixture
    let resp = client
        .get(format!("{}/company/04713697", CH_REST_BASE)) // Ofcom — stable public body
        .basic_auth(&key, Some(""))
        .send()
        .await
        .expect("CH REST request");

    assert_eq!(resp.status(), 200, "CH REST returned {}", resp.status());

    let body: serde_json::Value = resp.json().await.expect("CH REST JSON parse");
    eprintln!("✓ CH: company_name = {:?}", body["company_name"]);

    assert!(body["company_number"].is_string(), "company_number missing");
    assert!(body["company_name"].is_string(), "company_name missing");
    assert!(body["company_status"].is_string(), "company_status missing");
}

/// Fetch officers for a well-known company and assert the response is parseable.
#[tokio::test]
async fn live_ch_company_officers_list() {
    live_only!();
    let key = match ch_api_key() {
        Some(k) => k,
        None => {
            eprintln!("⏭  CH REST test skipped — URBAN__INGEST__CH_API_KEY not set");
            return;
        }
    };

    let client = http_client();
    let resp = client
        .get(format!("{}/company/04713697/officers", CH_REST_BASE))
        .basic_auth(&key, Some(""))
        .send()
        .await
        .expect("CH officers request");

    assert_eq!(resp.status(), 200, "CH officers returned {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("CH officers JSON");

    let items = body["items"].as_array().expect("items must be array");
    eprintln!("✓ CH: {} officers returned", items.len());
    assert!(!items.is_empty(), "Expected at least 1 officer");

    // Spot-check first officer has required fields
    let first = &items[0];
    assert!(first["name"].is_string(), "officer.name missing");
    assert!(
        first["officer_role"].is_string(),
        "officer.officer_role missing"
    );
}

/// Construct a synthetic CH streaming event from real CH REST data, run it
/// through the CH mapper, and assert a valid Entity mutation is produced.
#[tokio::test]
async fn live_ch_rest_data_maps_to_graph_mutation() {
    live_only!();
    let key = match ch_api_key() {
        Some(k) => k,
        None => {
            eprintln!("⏭  CH REST test skipped — URBAN__INGEST__CH_API_KEY not set");
            return;
        }
    };

    let client = http_client();
    let body: serde_json::Value = client
        .get(format!("{}/company/04713697", CH_REST_BASE))
        .basic_auth(&key, Some(""))
        .send()
        .await
        .expect("CH REST")
        .json()
        .await
        .expect("CH JSON");

    // Build a synthetic ChEvent matching the streaming format from REST data
    let company_number = body["company_number"].as_str().unwrap_or("04713697");
    let company_name = body["company_name"].as_str().unwrap_or("Unknown");
    let company_status = body["company_status"].as_str().unwrap_or("unknown");

    let event_json = serde_json::json!({
        "event": {
            "timepoint": 999999,
            "published_at": "2026-05-11T10:00:00+01:00",
            "fields_changed": ["company_status"],
            "type": "changed"
        },
        "resource_id": company_number,
        "resource_kind": "company-profile",
        "data": {
            "company_name": company_name,
            "company_number": company_number,
            "company_status": company_status
        }
    });

    let ch_event: ChEvent = serde_json::from_value(event_json).expect("ChEvent deserialise");
    let mutations = map_ch_event(&ch_event);

    assert!(
        !mutations.is_empty(),
        "CH mapper produced 0 mutations from real company data"
    );
    eprintln!(
        "✓ CH mapper: {} → {} IngestEvents (company_status={})",
        company_name,
        mutations.len(),
        company_status
    );

    // Assert at least one NodeUpsert(Entity) is in the IngestEvent result
    let has_entity = mutations.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::NodeUpsert {
                weight: urban_integrity_schema::types::node::NodeWeight::Entity(_),
                ..
            }
        )
    });
    assert!(
        has_entity,
        "Expected Entity NodeUpsert in mapper output for company-profile event"
    );

    // Also run through the pipeline to ensure no validation/normalisation errors
    let pipeline_results: Vec<_> = mutations.into_iter().map(process_event).collect();
    let errors: Vec<_> = pipeline_results.iter().filter(|r| r.is_err()).collect();
    assert!(
        errors.is_empty(),
        "Pipeline rejected {} events from real CH data: {:?}",
        errors.len(),
        errors
    );
    eprintln!(
        "✓ CH pipeline: all IngestEvents accepted, {} GraphMutations produced",
        pipeline_results.len()
    );
}
