//! Firecrawl-based fallback scraper for Idox PublicAccess planning portals.
//!
//! Called when the Scrapling sidecar is unavailable. Sends the portal URL to the
//! Firecrawl /v1/scrape API, receives back the rendered HTML, and parses Idox
//! `li.searchresult` items into [`PlanningPortalItem`] values.
//!
//! Requires a valid `URBAN__INGEST__FIRECRAWL_API_KEY` (or `firecrawl_api_key`
//! in config.toml).  Returns an empty Vec when the key is absent, the API fails,
//! or the response HTML contains no recognisable search results.

use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

use super::sidecar::PlanningPortalItem;

const FIRECRAWL_SCRAPE_URL: &str = "https://api.firecrawl.dev/v1/scrape";

#[derive(Serialize)]
struct FirecrawlRequest<'a> {
    url: &'a str,
    /// "rawHtml" fetches server-rendered HTML without JS execution — much faster for
    /// Idox portals which are server-side rendered. Falls back if empty.
    formats: &'a [&'a str],
    #[serde(rename = "onlyMainContent")]
    only_main_content: bool,
}

#[derive(Deserialize, Debug)]
struct FirecrawlResponse {
    success: Option<bool>,
    data: Option<FirecrawlData>,
    error: Option<String>,
}

#[derive(Deserialize, Debug)]
struct FirecrawlData {
    html: Option<String>,
    /// Firecrawl returns raw server HTML under "rawHtml" when that format is requested.
    #[serde(rename = "rawHtml")]
    raw_html: Option<String>,
}

/// Scrape an Idox PublicAccess portal URL via Firecrawl and parse the results list.
///
/// Returns an empty Vec on any error rather than propagating — caller degrades gracefully.
pub async fn scrape_idox_via_firecrawl(url: &str, api_key: &str) -> Vec<PlanningPortalItem> {
    if api_key.is_empty() {
        return vec![];
    }

    let client = match Client::builder().timeout(Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(?e, "firecrawl: failed to build reqwest client");
            return vec![];
        }
    };

    let body = FirecrawlRequest {
        url,
        formats: &["rawHtml"],
        only_main_content: false,
    };

    let resp = match client
        .post(FIRECRAWL_SCRAPE_URL)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(?e, url, "firecrawl: request failed");
            return vec![];
        }
    };

    let status = resp.status();
    if !status.is_success() {
        warn!(url, %status, "firecrawl: non-2xx response");
        return vec![];
    }

    let fc: FirecrawlResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(?e, url, "firecrawl: failed to deserialise response");
            return vec![];
        }
    };

    if fc.success != Some(true) {
        warn!(url, reason = ?fc.error, "firecrawl: API reported failure");
        return vec![];
    }

    let html = match fc.data {
        Some(d) => d
            .raw_html
            .filter(|s| !s.is_empty())
            .or_else(|| d.html.filter(|s| !s.is_empty()))
            .unwrap_or_default(),
        None => String::new(),
    };
    if html.is_empty() {
        warn!(url, "firecrawl: response contained no HTML");
        return vec![];
    }

    let items = parse_idox_html(&html);
    debug!(
        url,
        count = items.len(),
        "firecrawl: parsed items from HTML"
    );
    items
}

/// Parse Idox PublicAccess search-results HTML into [`PlanningPortalItem`] values.
///
/// Selector targets (standard across all Idox v6/v7 deployments):
/// - `li.searchresult` — one per planning application
/// - `p.metaInfo`       — contains the application reference
/// - `.summaryLinkTextClamp` or `.summaryLinkText` — description
/// - `p.address`        — site address
///
/// Applicant and ward are left `None` — they require a detail-page fetch which
/// the sidecar handles. The mapper will substitute "Unknown Applicant" for the entity
/// slug, and ward_code is supplied by the portal config.
fn parse_idox_html(html: &str) -> Vec<PlanningPortalItem> {
    let document = Html::parse_document(html);

    let Ok(li_sel) = Selector::parse("li.searchresult") else {
        return vec![];
    };
    let ref_sel = Selector::parse("p.metaInfo").ok();
    let desc_sel = Selector::parse(".summaryLinkTextClamp, .summaryLinkText, p.description").ok();
    let addr_sel = Selector::parse("p.address").ok();

    let mut items = Vec::new();

    for li in document.select(&li_sel) {
        let reference = ref_sel
            .as_ref()
            .and_then(|s| li.select(s).next())
            .map(|el| el.text().collect::<String>().trim().to_owned())
            .filter(|s| !s.is_empty());

        if reference.is_none() {
            continue; // mapper requires a reference
        }

        let description = desc_sel
            .as_ref()
            .and_then(|s| li.select(s).next())
            .map(|el| el.text().collect::<String>().trim().to_owned())
            .filter(|s| !s.is_empty());

        let address = addr_sel
            .as_ref()
            .and_then(|s| li.select(s).next())
            .map(|el| el.text().collect::<String>().trim().to_owned())
            .filter(|s| !s.is_empty());

        items.push(PlanningPortalItem {
            reference,
            description,
            address,
            ward: None,
            applicant: None,
            received_date: None,
            applicant_provenance: None,
            detail_handle: None,
        });
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_idox_search_result_html() {
        let html = r#"
        <html><body>
        <ul id="searchresults">
          <li class="searchresult">
            <p class="metaInfo">DC/2024/00123</p>
            <p class="summaryLinkText">Erection of single storey rear extension</p>
            <p class="address">12 High Street, Bearwood, B66 1AA</p>
          </li>
          <li class="searchresult">
            <p class="metaInfo">FUL/2024/00456</p>
            <p class="summaryLinkTextClamp">Change of use from A1 to A3</p>
            <p class="address">45 Market Place, Bearwood</p>
          </li>
          <li class="searchresult">
            <!-- no metaInfo — should be skipped -->
            <p class="address">Orphan row</p>
          </li>
        </ul>
        </body></html>
        "#;
        let items = parse_idox_html(html);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].reference.as_deref(), Some("DC/2024/00123"));
        assert_eq!(
            items[0].address.as_deref(),
            Some("12 High Street, Bearwood, B66 1AA")
        );
        assert_eq!(items[1].reference.as_deref(), Some("FUL/2024/00456"));
        assert_eq!(
            items[1].description.as_deref(),
            Some("Change of use from A1 to A3")
        );
        assert!(items[0].applicant.is_none());
    }

    #[test]
    fn returns_empty_when_no_results() {
        let items = parse_idox_html("<html><body><p>No results found</p></body></html>");
        assert!(items.is_empty());
    }

    #[test]
    fn skips_results_without_reference() {
        let html = r#"
        <ul>
          <li class="searchresult"><p class="address">No ref here</p></li>
        </ul>
        "#;
        assert!(parse_idox_html(html).is_empty());
    }
}
