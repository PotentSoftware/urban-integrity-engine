use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::warn;

/// Fallback per-request timeout when no configured value is threaded through.
/// Kept in step with `crates/core/src/config.rs::default_scrapling_timeout_secs`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Request sent to the Scrapling sidecar's `/scrape` endpoint.
#[derive(Serialize, Debug)]
pub struct ScrapeRequest {
    pub url: String,
    /// CSS selector map:
    /// - First entry: container selector (the repeating item element)
    /// - Subsequent entries: field selectors relative to each container
    pub selectors: HashMap<String, String>,
    /// Per-request timeout in seconds (default: 30)
    pub timeout: u64,
    /// Portal system type — drives sidecar scraping strategy.
    /// Values: "idox" | "arcus" | "wfs" | "planapi"
    /// Defaults to "idox" (~69% of UK LPAs use Idox PublicAccess) — the default
    /// lives in the deserialize-side config at `crates/core/src/config.rs`.
    pub portal_type: String,
}

/// Response from the Scrapling sidecar's `/scrape` endpoint.
#[derive(Deserialize, Debug)]
pub struct ScrapeResponse {
    pub status: String,
    pub results: Option<Vec<HashMap<String, Option<String>>>>,
    pub reason: Option<String>,
}

/// A scraped planning portal application item.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanningPortalItem {
    pub reference: Option<String>,
    pub description: Option<String>,
    pub address: Option<String>,
    /// Ward name returned by the sidecar (populated for Idox detail-page fetches)
    pub ward: Option<String>,
    /// Applicant name returned by the sidecar (populated for Idox detail-page fetches)
    pub applicant: Option<String>,
    /// Validated/received date (ISO YYYY-MM-DD) from the sidecar (D-07). None when the portal
    /// is a stub or the date is absent or the portal does not emit a date.
    pub received_date: Option<String>,
    /// Applicant provenance wire string from the sidecar (D-02): one of "published" |
    /// "absent_at_source" | "fetch_failed", or None for portals/stubs that don't emit it yet.
    pub applicant_provenance: Option<String>,
    /// Opaque portal-assigned handle for the application detail page (D-06), used by 24-05's
    /// backfill to re-fetch a specific record. None for portals/stubs that don't emit it yet.
    pub detail_handle: Option<String>,
}

/// Why a `/scrape` call produced no items.
///
/// Distinguishing these from a genuinely empty portal response is load-bearing for
/// `crates/ingestor/src/backfill/planning.rs`: an empty result is evidence about the SOURCE
/// (these references are gone), whereas a failure is evidence about US. Conflating them let a
/// single client-side timeout stamp 269 verifiably-`absent_at_source` Contracts as
/// `fetch_failed` (found by the Phase 24 UAT run, 2026-07-27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrapeFailure {
    /// The request never completed: timeout, connection refused, DNS, TLS.
    Transport,
    /// The sidecar answered, but reported `status != "ok"`.
    ErrorStatus,
    /// The sidecar answered with a body this client could not parse.
    MalformedResponse,
}

/// Outcome of a `/scrape` call.
///
/// `Items(vec![])` means the portal genuinely returned nothing. `Failed(_)` means we do not
/// know what the portal holds. Callers that write data derived from absence MUST branch on
/// this rather than on `Vec::is_empty()`.
#[derive(Debug)]
pub enum ScrapeOutcome {
    Items(Vec<PlanningPortalItem>),
    Failed(ScrapeFailure),
}

impl ScrapeOutcome {
    /// Collapse to the legacy best-effort shape: a failure becomes an empty Vec.
    ///
    /// Only correct for callers that treat portal data as purely additive (the poller, the
    /// on-demand ingest). Never use it where absence drives a write.
    pub fn items_or_empty(self) -> Vec<PlanningPortalItem> {
        match self {
            ScrapeOutcome::Items(items) => items,
            ScrapeOutcome::Failed(_) => vec![],
        }
    }
}

/// Rust client for the Scrapling sidecar service.
///
/// On any network error or sidecar-reported error, returns an empty Vec
/// and logs a warning — never panics. The caller continues without portal data.
pub struct ScrapingSidecar {
    client: Client,
    pub base_url: String,
    timeout_secs: u64,
}

impl ScrapingSidecar {
    /// Build a client with the default request timeout
    /// (`IngestConfig::scrapling_timeout_secs`'s default).
    pub fn new(url: &str, port: u16) -> Self {
        Self::with_timeout(url, port, DEFAULT_TIMEOUT_SECS)
    }

    /// Build a client with an explicit per-request timeout.
    ///
    /// A ward re-list is ONE request covering every application in the ward, so this ceiling
    /// must clear the slowest ward rather than the average one — see
    /// `crates/core/src/config.rs::default_scrapling_timeout_secs` for the measured numbers.
    pub fn with_timeout(url: &str, port: u16, timeout_secs: u64) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(timeout_secs))
                .build()
                .expect("reqwest client"),
            base_url: format!("{}:{}", url, port),
            timeout_secs,
        }
    }

    /// Returns `true` if the sidecar is reachable and healthy.
    pub async fn is_healthy(&self) -> bool {
        self.client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Scrape a planning portal URL.
    ///
    /// # Arguments
    /// * `url` - the full URL to POST to the sidecar
    /// * `portal_type` - the portal system type ("idox", "arcus", "wfs", "planapi")
    ///
    /// Returns an empty Vec if the sidecar is unavailable, times out, or returns an error.
    /// This method is designed to degrade gracefully — portal scraping is best-effort.
    ///
    /// Callers that write data derived from an EMPTY result must use [`Self::scrape_checked`]
    /// instead: this signature cannot tell "the portal has nothing" from "we failed to ask".
    pub async fn scrape(&self, url: &str, portal_type: &str) -> Vec<PlanningPortalItem> {
        self.scrape_checked(url, portal_type).await.items_or_empty()
    }

    /// Scrape a planning portal URL, preserving WHY a call yielded nothing.
    ///
    /// See [`ScrapeOutcome`]. Prefer this over [`Self::scrape`] wherever absence of results
    /// leads to a persisted claim.
    pub async fn scrape_checked(&self, url: &str, portal_type: &str) -> ScrapeOutcome {
        // Selectors are used as hints by the sidecar for generic CSS portals.
        // For Idox portals, the sidecar uses fixed well-known selectors and ignores this map.
        let mut selectors = HashMap::new();
        selectors.insert("container".to_string(), "li.searchresult".to_string());
        selectors.insert("reference".to_string(), "p.metaInfo".to_string());
        selectors.insert(
            "description".to_string(),
            ".summaryLinkTextClamp".to_string(),
        );
        selectors.insert("address".to_string(), "p.address".to_string());

        let request = ScrapeRequest {
            url: url.to_string(),
            selectors,
            timeout: 30,
            portal_type: portal_type.to_string(),
        };

        let resp = match self
            .client
            .post(format!("{}/scrape", self.base_url))
            .json(&request)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                // Distinct from "unreachable": the sidecar is up and working, we simply
                // stopped waiting. Naming it a timeout — and printing the ceiling we gave up
                // at — points the operator at `ingest.scrapling_timeout_secs` instead of
                // sending them to check a healthy service.
                warn!(
                    url,
                    timeout_secs = self.timeout_secs,
                    "Scrapling sidecar request timed out — raise ingest.scrapling_timeout_secs \
                     if this URL legitimately takes longer"
                );
                return ScrapeOutcome::Failed(ScrapeFailure::Transport);
            }
            Err(e) => {
                warn!(
                    url,
                    ?e,
                    "Scrapling sidecar unreachable — skipping portal scrape"
                );
                return ScrapeOutcome::Failed(ScrapeFailure::Transport);
            }
        };

        match resp.json::<ScrapeResponse>().await {
            Ok(r) if r.status == "ok" => ScrapeOutcome::Items(
                r.results
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mut m| PlanningPortalItem {
                        reference: m.remove("reference").flatten(),
                        description: m.remove("description").flatten(),
                        address: m.remove("address").flatten(),
                        ward: m.remove("ward").flatten(),
                        applicant: m.remove("applicant").flatten(),
                        received_date: m.remove("received_date").flatten(),
                        applicant_provenance: m.remove("applicant_provenance").flatten(),
                        detail_handle: m.remove("detail_handle").flatten(),
                    })
                    .collect(),
            ),
            Ok(r) => {
                warn!(
                    url,
                    reason = ?r.reason,
                    "Scrapling sidecar returned error status — skipping"
                );
                ScrapeOutcome::Failed(ScrapeFailure::ErrorStatus)
            }
            Err(e) => {
                warn!(?e, "Failed to parse Scrapling sidecar response");
                ScrapeOutcome::Failed(ScrapeFailure::MalformedResponse)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sidecar_new_builds_correct_base_url() {
        let sidecar = ScrapingSidecar::new("http://localhost", 8765);
        assert_eq!(sidecar.base_url, "http://localhost:8765");
    }

    #[test]
    fn test_scrape_response_deserialization_ok() {
        let json = r#"{
            "status": "ok",
            "results": [
                {
                    "reference": "23/01234",
                    "description": "Warehouse erection",
                    "address": "LS1 1AA"
                }
            ]
        }"#;
        let resp: ScrapeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.results.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_scrape_response_deserialization_error() {
        let json = r#"{"status": "error", "reason": "timeout"}"#;
        let resp: ScrapeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.reason.as_deref(), Some("timeout"));
    }

    #[test]
    fn test_with_timeout_records_the_configured_ceiling() {
        let sidecar = ScrapingSidecar::with_timeout("http://localhost", 8765, 420);
        assert_eq!(
            sidecar.timeout_secs, 420,
            "the configured timeout must be retained for the log line that reports it"
        );
        assert_eq!(
            ScrapingSidecar::new("http://localhost", 8765).timeout_secs,
            DEFAULT_TIMEOUT_SECS,
            "new() must fall back to the shared default, not a second hardcoded value"
        );
    }

    #[tokio::test]
    async fn test_scrape_checked_reports_transport_failure_not_empty_items() {
        // The whole point of the checked variant: a call that never completed must NOT be
        // indistinguishable from a portal that returned zero rows.
        let sidecar = ScrapingSidecar::new("http://127.0.0.1", 19999);
        match sidecar.scrape_checked("http://example.com", "idox").await {
            ScrapeOutcome::Failed(ScrapeFailure::Transport) => {}
            other => panic!("expected Failed(Transport), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_scrape_checked_reports_ok_with_zero_results_as_items() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"status":"ok","results":[]}"#;
        let body_owned = body.to_owned();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_owned.len(),
                    body_owned
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let sidecar = ScrapingSidecar::new("http://127.0.0.1", port);
        match sidecar.scrape_checked("http://example.com", "idox").await {
            ScrapeOutcome::Items(items) => assert!(items.is_empty()),
            other => panic!("a successful empty response is an ANSWER, not a failure: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_scrape_checked_reports_error_status_and_malformed_distinctly() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        async fn outcome_for(body: &'static str) -> ScrapeOutcome {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            ScrapingSidecar::new("http://127.0.0.1", port)
                .scrape_checked("http://example.com", "idox")
                .await
        }

        match outcome_for(r#"{"status":"error","reason":"portal-blocked","results":null}"#).await {
            ScrapeOutcome::Failed(ScrapeFailure::ErrorStatus) => {}
            other => panic!("expected Failed(ErrorStatus), got {other:?}"),
        }
        match outcome_for("not-valid-json").await {
            ScrapeOutcome::Failed(ScrapeFailure::MalformedResponse) => {}
            other => panic!("expected Failed(MalformedResponse), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_scrape_returns_empty_on_unreachable_sidecar() {
        // Use port 19999 — not the actual sidecar port, avoids conflict with any running instance
        let sidecar = ScrapingSidecar::new("http://127.0.0.1", 19999);
        let result = sidecar.scrape("http://example.com", "idox").await;
        assert!(
            result.is_empty(),
            "Unavailable sidecar should yield empty results, not panic"
        );
    }

    #[tokio::test]
    async fn test_health_returns_false_on_unreachable_sidecar() {
        let sidecar = ScrapingSidecar::new("http://127.0.0.1", 19999);
        let healthy = sidecar.is_healthy().await;
        assert!(!healthy, "Unreachable sidecar should not be healthy");
    }

    #[tokio::test]
    async fn test_scrape_returns_items_from_mock_ok_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // Bind on port 0 — OS assigns a free ephemeral port
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"status":"ok","results":[{"reference":"23/01234","description":"Warehouse erection","address":"LS1 1AA","ward":null,"applicant":null}]}"#;
        let body_owned = body.to_owned();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request so reqwest doesn't get a connection reset
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_owned.len(),
                    body_owned
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let sidecar = ScrapingSidecar::new("http://127.0.0.1", port);
        let items = sidecar.scrape("http://example.com/planning", "idox").await;
        assert!(
            !items.is_empty(),
            "mock ok response must yield at least one item"
        );
        assert_eq!(items[0].reference.as_deref(), Some("23/01234"));
        assert_eq!(items[0].description.as_deref(), Some("Warehouse erection"));
        assert_eq!(items[0].address.as_deref(), Some("LS1 1AA"));
        assert!(items[0].ward.is_none());
        assert!(items[0].applicant.is_none());
    }

    #[tokio::test]
    async fn test_scrape_returns_empty_on_error_status_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"status":"error","reason":"portal-blocked","results":null}"#;
        let body_owned = body.to_owned();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_owned.len(),
                    body_owned
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let sidecar = ScrapingSidecar::new("http://127.0.0.1", port);
        let items = sidecar.scrape("http://example.com/planning", "idox").await;
        assert!(
            items.is_empty(),
            "error-status response must yield empty results"
        );
    }

    #[tokio::test]
    async fn test_scrape_returns_empty_on_malformed_json_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "not-valid-json";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let sidecar = ScrapingSidecar::new("http://127.0.0.1", port);
        let items = sidecar.scrape("http://example.com/planning", "idox").await;
        assert!(
            items.is_empty(),
            "malformed JSON response must yield empty results"
        );
    }

    #[tokio::test]
    async fn test_scrape_handles_ward_and_applicant_fields() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"status":"ok","results":[{"reference":"24/00567","description":"Loft conversion","address":"B1 1AA","ward":"Sandwell Central","applicant":"John Doe Ltd"}]}"#;
        let body_owned = body.to_owned();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_owned.len(),
                    body_owned
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let sidecar = ScrapingSidecar::new("http://127.0.0.1", port);
        let items = sidecar.scrape("http://example.com/planning", "idox").await;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].ward.as_deref(), Some("Sandwell Central"));
        assert_eq!(items[0].applicant.as_deref(), Some("John Doe Ltd"));
    }
}
