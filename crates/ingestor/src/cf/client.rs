use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;
use tracing::{debug, info, warn};

use urban_integrity_core::{graph::coordinator::GraphCoordinatorHandle, pipeline::process};

use crate::cf::{mapper::map_release, types::OcdsPackage};

const CF_BASE_URL: &str =
    "https://www.contractsfinder.service.gov.uk/Published/Notices/OCDS/Search";

/// Cursor store for Contracts Finder publishedDate tracking.
/// Mirrors the pattern from `ch::client::CursorStore` for consistency.
#[async_trait::async_trait]
pub trait CfCursorStore: Send + Sync {
    /// Load the last publishedDate cursor (ISO8601 string)
    async fn load_cursor(&self) -> Option<String>;
    /// Persist the latest publishedDate seen this poll cycle
    async fn persist_cursor(&self, date: &str);
}

/// In-memory cursor store for testing.
pub struct InMemoryCfCursor {
    date: tokio::sync::Mutex<Option<String>>,
}

impl Default for InMemoryCfCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryCfCursor {
    pub fn new() -> Self {
        Self {
            date: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl CfCursorStore for InMemoryCfCursor {
    async fn load_cursor(&self) -> Option<String> {
        self.date.lock().await.clone()
    }
    async fn persist_cursor(&self, date: &str) {
        *self.date.lock().await = Some(date.to_string());
    }
}

/// Contracts Finder REST polling client.
///
/// Polls the OCDS search endpoint at `publishedFrom` cursor position,
/// follows `links.next` pagination within each poll cycle, and forwards
/// `GraphMutation`s to the coordinator.
pub struct CfPoller<C: CfCursorStore> {
    client: Client,
    coordinator: GraphCoordinatorHandle,
    cursor: C,
    poll_interval: Duration,
    api_base: String,
}

impl<C: CfCursorStore + 'static> CfPoller<C> {
    pub fn new(coordinator: GraphCoordinatorHandle, cursor: C, poll_interval_secs: u64) -> Self {
        Self::new_with_base(
            coordinator,
            cursor,
            poll_interval_secs,
            CF_BASE_URL.to_string(),
        )
    }

    fn new_with_base(
        coordinator: GraphCoordinatorHandle,
        cursor: C,
        poll_interval_secs: u64,
        api_base: String,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            coordinator,
            cursor,
            poll_interval: Duration::from_secs(poll_interval_secs),
            api_base,
        }
    }

    /// Poll Contracts Finder on an interval forever.
    pub async fn run(&self) {
        loop {
            if let Err(e) = self.poll_once().await {
                warn!(?e, "CF poll error — will retry after interval");
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn poll_once(&self) -> Result<()> {
        let cursor = self.cursor.load_cursor().await;
        let mut url = match cursor {
            Some(ref from) => format!("{}?publishedFrom={}&limit=100", self.api_base, from),
            None => format!("{}?limit=100", self.api_base),
        };

        let mut page = 0usize;
        let mut latest_date: Option<String> = None;

        loop {
            info!(page, url = %url, "CF poll: fetching page");
            let resp = self.client.get(&url).send().await.context("CF fetch")?;
            resp.error_for_status_ref().context("CF bad status")?;

            let package: OcdsPackage = resp.json().await.context("CF JSON parse")?;
            let release_count = package.releases.len();
            debug!(release_count, page, "CF releases received");

            for release in &package.releases {
                // Track latest date for cursor advancement
                if let Some(dt) = release.date.as_deref() {
                    match &latest_date {
                        None => latest_date = Some(dt.to_string()),
                        Some(prev) if dt > prev.as_str() => {
                            latest_date = Some(dt.to_string());
                        }
                        _ => {}
                    }
                }

                for ingest_event in map_release(release) {
                    match process(ingest_event) {
                        Ok(mutation) => {
                            if let Err(e) = self.coordinator.mutation_tx.send(mutation).await {
                                warn!(?e, "CF coordinator send failed — channel closed?");
                            }
                        }
                        Err(e) => debug!(?e, "CF pipeline rejected event"),
                    }
                }
            }

            // Persist cursor after each page
            if let Some(ref dt) = latest_date {
                self.cursor.persist_cursor(dt).await;
            }

            // Follow pagination — stop when no next link or empty page
            match package.links.and_then(|l| l.next) {
                Some(next) if release_count > 0 => {
                    url = next;
                    page += 1;
                }
                _ => break,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::{
        graph::coordinator::{CoordinatorConfig, GraphCoordinator},
        persistence::wal::wal_channel,
    };

    fn make_coordinator() -> GraphCoordinatorHandle {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        handle
    }

    #[tokio::test]
    async fn test_inmemory_cf_cursor_default_is_none() {
        let cursor = InMemoryCfCursor::default();
        assert!(cursor.load_cursor().await.is_none());
    }

    #[tokio::test]
    async fn test_inmemory_cf_cursor_persist_and_load() {
        let cursor = InMemoryCfCursor::new();
        cursor.persist_cursor("2026-01-15").await;
        assert_eq!(cursor.load_cursor().await.as_deref(), Some("2026-01-15"));
    }

    #[tokio::test]
    async fn test_cfpoller_poll_once_returns_err_on_unreachable_server() {
        let handle = make_coordinator();
        let cursor = InMemoryCfCursor::new();
        let poller =
            CfPoller::new_with_base(handle, cursor, 999, "http://127.0.0.1:19998".to_string());
        let result = poller.poll_once().await;
        assert!(result.is_err(), "Unreachable server should return Err");
    }

    #[tokio::test]
    async fn test_cfpoller_poll_once_empty_releases_via_mock_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"releases":[],"links":null}"#;
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

        let handle = make_coordinator();
        let cursor = InMemoryCfCursor::new();
        let poller =
            CfPoller::new_with_base(handle, cursor, 999, format!("http://127.0.0.1:{}", port));
        let result = poller.poll_once().await;
        assert!(
            result.is_ok(),
            "Empty releases should succeed: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_cfpoller_poll_once_with_cursor_advances_url() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"releases":[],"links":null}"#;
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

        let handle = make_coordinator();
        let cursor = InMemoryCfCursor::new();
        cursor.persist_cursor("2026-01-01").await;
        let poller =
            CfPoller::new_with_base(handle, cursor, 999, format!("http://127.0.0.1:{}", port));
        let result = poller.poll_once().await;
        assert!(result.is_ok(), "Cursor branch should succeed: {:?}", result);
    }

    #[tokio::test]
    async fn test_cfpoller_poll_once_with_single_release_via_mock_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"releases":[{"ocid":"ocds-abc-001","id":"test-001","date":"2026-01-15T12:00:00Z"}],"links":null}"#;
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

        let handle = make_coordinator();
        let cursor = InMemoryCfCursor::new();
        let poller =
            CfPoller::new_with_base(handle, cursor, 999, format!("http://127.0.0.1:{}", port));
        let result = poller.poll_once().await;
        assert!(
            result.is_ok(),
            "Single release should succeed: {:?}",
            result
        );
    }
}
