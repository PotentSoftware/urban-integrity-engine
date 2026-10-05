use anyhow::{Context, Result};
use futures::stream::TryStreamExt;
use reqwest::Client;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
use tracing::{debug, error, info, warn};

use urban_integrity_core::{graph::coordinator::GraphCoordinatorHandle, pipeline::process};

use crate::ch::{mapper::map_ch_event, types::ChEvent};

// Re-export so existing code using `crate::ch::client::CursorStore` or `InMemoryCursorStore` compiles.
pub use urban_integrity_core::stream::{CursorStore, InMemoryCursorStore};

const CH_STREAM_BASE: &str = "https://stream.companieshouse.gov.uk/companies";

/// The officer-appointment stream — a **separate** endpoint from `/companies`.
///
/// `map_ch_event` has always handled `resource_kind: "officer-appointment"`, but nothing ever
/// subscribed to the stream that emits it, so `map_officer` was unreachable in production and the
/// graph held zero Officer nodes. Measured 2026-07-29 against `an_anglesey_aebaa785`: 358,829
/// NodeAdded events across 8 node types, **not one of them an Officer**.
///
/// That single gap disables more than the `officer` table. `Phoenix` motif detection asserts on
/// `NodeWeight::Officer` and can never fire; `ch_disqualified` builds its candidate set from
/// Officer nodes and recorded `candidates_found: 0` on every analysis; `ch_psc`'s officer match
/// falls through to a standalone Psc node with no identity link.
const CH_OFFICERS_STREAM_BASE: &str = "https://stream.companieshouse.gov.uk/officers";

const MAX_BACKOFF_SECS: u64 = 60;

/// Companies House streaming client.
///
/// Connects to the CH NDJSON stream, parses events line-by-line,
/// persists the timepoint cursor on every event, and forwards
/// `GraphMutation`s to the `GraphCoordinatorHandle`.
///
/// Reconnects with exponential backoff on any error.
pub struct ChStreamClient<C: CursorStore> {
    client: Client,
    api_key: String,
    coordinator: GraphCoordinatorHandle,
    pub(crate) cursor: C,
    stream_base: String,
}

impl<C: CursorStore + 'static> ChStreamClient<C> {
    /// Client for the company-profile stream (`/companies`).
    pub fn new(api_key: String, coordinator: GraphCoordinatorHandle, cursor: C) -> Self {
        Self::new_with_base(api_key, coordinator, cursor, CH_STREAM_BASE.to_string())
    }

    /// Client for the officer-appointment stream (`/officers`).
    ///
    /// Pass a cursor distinct from the company stream's — see
    /// `SurrealCursorStore::new_officers`. Both streams run as separate long-lived connections;
    /// CH treats them independently and so must we.
    pub fn new_officers(api_key: String, coordinator: GraphCoordinatorHandle, cursor: C) -> Self {
        Self::new_with_base(
            api_key,
            coordinator,
            cursor,
            CH_OFFICERS_STREAM_BASE.to_string(),
        )
    }

    pub(crate) fn new_with_base(
        api_key: String,
        coordinator: GraphCoordinatorHandle,
        cursor: C,
        stream_base: String,
    ) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .tcp_keepalive(Duration::from_secs(60))
                .build()
                .expect("reqwest client"),
            api_key,
            coordinator,
            cursor,
            stream_base,
        }
    }

    /// Run forever — reconnects on error with exponential backoff.
    pub async fn run(&self) {
        let mut backoff_secs = 1u64;
        loop {
            match self.connect_and_stream().await {
                Ok(()) => {
                    info!("CH stream ended cleanly — reconnecting");
                    backoff_secs = 1;
                }
                Err(e) => {
                    error!(?e, "CH stream error");
                    warn!("Backing off {}s before reconnect", backoff_secs);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                }
            }
        }
    }

    pub(crate) async fn connect_and_stream(&self) -> Result<()> {
        let timepoint = self.cursor.load_timepoint().await.unwrap_or(0);

        let url = if timepoint > 0 {
            format!("{}?timepoint={}", self.stream_base, timepoint)
        } else {
            self.stream_base.clone()
        };

        info!(timepoint, "Connecting to CH stream at {}", url);

        let response = self
            .client
            .get(&url)
            .basic_auth(&self.api_key, None::<&str>)
            .send()
            .await
            .context("CH stream connect")?;

        // 416 → timepoint too old — clear and reconnect fresh
        if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            warn!("CH timepoint too old (416 Range Not Satisfiable) — clearing cursor and reconnecting");
            self.cursor.clear_timepoint().await;
            return Ok(());
        }

        response
            .error_for_status_ref()
            .context("CH stream bad status")?;

        let byte_stream = response.bytes_stream().map_err(std::io::Error::other);
        let mut lines = BufReader::new(StreamReader::new(byte_stream)).lines();

        while let Some(line) = lines.next_line().await.context("CH stream read")? {
            if line.trim().is_empty() {
                continue; // heartbeat empty lines
            }

            match serde_json::from_str::<ChEvent>(&line) {
                Ok(event) => {
                    let tp = event.event.timepoint;
                    self.cursor.persist_timepoint(tp).await;

                    for ingest_event in map_ch_event(&event) {
                        match process(ingest_event) {
                            Ok(mutation) => {
                                if let Err(e) = self.coordinator.mutation_tx.send(mutation).await {
                                    warn!(?e, "CH coordinator send failed — channel closed?");
                                }
                            }
                            Err(e) => debug!(?e, "CH pipeline rejected event"),
                        }
                    }
                }
                Err(e) => {
                    debug!(?e, line = %line, "CH JSON parse error — skipping line");
                }
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
    async fn test_inmemory_cursor_store_default_is_none() {
        let store = InMemoryCursorStore::default();
        assert!(store.load_timepoint().await.is_none());
    }

    #[tokio::test]
    async fn test_inmemory_cursor_store_persist_and_load() {
        let store = InMemoryCursorStore::new();
        store.persist_timepoint(42).await;
        assert_eq!(store.load_timepoint().await, Some(42));
    }

    #[tokio::test]
    async fn test_inmemory_cursor_store_clear_resets_to_none() {
        let store = InMemoryCursorStore::new();
        store.persist_timepoint(100).await;
        store.clear_timepoint().await;
        assert!(store.load_timepoint().await.is_none());
    }

    /// The company and officer clients must target different CH endpoints.
    ///
    /// This is the regression that made `map_officer` dead code: it had unit coverage and a
    /// dispatch arm, but the only client ever constructed pointed at `/companies`, so no
    /// `officer-appointment` event could arrive. Coverage of a mapper proves nothing if nothing
    /// feeds it.
    #[test]
    fn the_officer_client_subscribes_to_a_different_stream_than_the_company_client() {
        let companies = ChStreamClient::new(
            "k".to_string(),
            make_coordinator_blocking(),
            InMemoryCursorStore::new(),
        );
        let officers = ChStreamClient::new_officers(
            "k".to_string(),
            make_coordinator_blocking(),
            InMemoryCursorStore::new(),
        );
        assert_ne!(
            companies.stream_base, officers.stream_base,
            "both clients point at the same endpoint — one of the two streams is not subscribed"
        );
        assert!(
            officers.stream_base.ends_with("/officers"),
            "officer client must target CH's officer-appointment stream, got {}",
            officers.stream_base
        );
        assert!(
            companies.stream_base.ends_with("/companies"),
            "company client must target CH's company-profile stream, got {}",
            companies.stream_base
        );
    }

    /// `make_coordinator` needs a Tokio runtime for its spawn; this is the sync-test equivalent.
    fn make_coordinator_blocking() -> GraphCoordinatorHandle {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (_coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        handle
    }

    #[tokio::test]
    async fn test_connect_and_stream_returns_err_on_unreachable() {
        let handle = make_coordinator();
        let cursor = InMemoryCursorStore::new();
        let client = ChStreamClient::new_with_base(
            "testkey".to_string(),
            handle,
            cursor,
            "http://127.0.0.1:19997".to_string(),
        );
        let result = client.connect_and_stream().await;
        assert!(result.is_err(), "Unreachable server should return Err");
    }

    #[tokio::test]
    async fn test_connect_and_stream_with_timepoint_in_url() {
        let handle = make_coordinator();
        let cursor = InMemoryCursorStore::new();
        cursor.persist_timepoint(999).await;
        let client = ChStreamClient::new_with_base(
            "testkey".to_string(),
            handle,
            cursor,
            "http://127.0.0.1:19997".to_string(),
        );
        let result = client.connect_and_stream().await;
        assert!(
            result.is_err(),
            "Unreachable with timepoint should return Err"
        );
    }

    #[tokio::test]
    async fn test_connect_and_stream_clears_cursor_on_416_response() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response =
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let handle = make_coordinator();
        let cursor = InMemoryCursorStore::new();
        cursor.persist_timepoint(12345).await;
        let client = ChStreamClient::new_with_base(
            "testkey".to_string(),
            handle,
            cursor,
            format!("http://127.0.0.1:{}", port),
        );
        let result = client.connect_and_stream().await;
        assert!(result.is_ok(), "416 should clear cursor and return Ok");
        assert!(
            client.cursor.load_timepoint().await.is_none(),
            "Cursor should be cleared after 416"
        );
    }

    #[tokio::test]
    async fn test_connect_and_stream_parses_ndjson_events() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Valid CH event NDJSON + empty heartbeat line
        let ndjson = concat!(
            "\n",
            r#"{"event":{"timepoint":42,"published_at":"2026-01-01T00:00:00Z","type":"changed"},"resource_id":"12345678","resource_kind":"company-profile","data":{"company_name":"Test Corp","company_number":"12345678","company_status":"active"}}"#,
            "\n",
            r#"not-valid-json-line"#,
            "\n",
        );
        let body = ndjson.to_string();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let handle = make_coordinator();
        let cursor = InMemoryCursorStore::new();
        let client = ChStreamClient::new_with_base(
            "testkey".to_string(),
            handle,
            cursor,
            format!("http://127.0.0.1:{}", port),
        );
        let result = client.connect_and_stream().await;
        assert!(result.is_ok(), "Valid NDJSON stream should return Ok");
        assert_eq!(
            client.cursor.load_timepoint().await,
            Some(42),
            "Cursor should be updated to parsed timepoint"
        );
    }
}
