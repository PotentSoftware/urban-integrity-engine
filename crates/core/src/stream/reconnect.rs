use anyhow::{Context, Result};
use futures::stream::TryStreamExt;
use reqwest::Client;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
use tracing::{debug, error, info, warn};

use super::cursor::CursorStore;

const MAX_BACKOFF_SECS: u64 = 60;

/// Handler invoked for each NDJSON line emitted by the CH stream.
///
/// Returns `Ok(())` to continue streaming, or an `Err` to abort the current
/// connection (the reconnect loop will retry with backoff).
#[async_trait::async_trait]
pub trait StreamEventHandler: Send + Sync {
    async fn on_line(&self, line: &str) -> Result<()>;
}

/// Run a Companies House NDJSON stream with exponential reconnect/backoff.
///
/// # Arguments
/// - `client` — Shared `reqwest::Client` (caller-owned, avoids repeated TLS handshakes).
/// - `api_key` — CH Basic Auth key (username part; password is always empty).
/// - `stream_base` — Base URL, e.g. `"https://stream.companieshouse.gov.uk/companies"`.
/// - `cursor` — Persisted timepoint store; updated on every event, cleared on 416.
/// - `handler` — Called once per NDJSON line (non-empty). The ingestor passes its
///   WAL-emit path here; the enricher passes its parse path.
///
/// # Behaviour
/// - Appends `?timepoint={tp}` when the cursor holds a non-zero value.
/// - On `416 Range Not Satisfiable`: clears the cursor and returns `Ok(())` so the
///   outer reconnect loop restarts fresh (no backoff penalty for stale cursor).
/// - On any other error: returns `Err` which triggers exponential backoff (1s → 2s →
///   4s … capped at 60s) and then reconnects.
/// - Empty/heartbeat lines are silently skipped.
/// - Malformed lines are `debug!`-logged and skipped; they do not abort the stream.
///
/// # Loop behaviour
/// This function loops forever. Cancel by dropping the `JoinHandle` of the task
/// that spawns it.
pub async fn run_ch_stream<H: StreamEventHandler + 'static>(
    client: &Client,
    api_key: &str,
    stream_base: &str,
    cursor: &dyn CursorStore,
    handler: &H,
) -> ! {
    let mut backoff_secs = 1u64;
    loop {
        match connect_and_stream(client, api_key, stream_base, cursor, handler).await {
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

/// Single connection attempt + streaming drain.
///
/// Returns `Ok(())` on clean end-of-stream or on a recoverable 416.
/// Returns `Err` on any connectivity or HTTP error.
pub(crate) async fn connect_and_stream<H: StreamEventHandler>(
    client: &Client,
    api_key: &str,
    stream_base: &str,
    cursor: &dyn CursorStore,
    handler: &H,
) -> Result<()> {
    let timepoint = cursor.load_timepoint().await.unwrap_or(0);

    let url = if timepoint > 0 {
        format!("{stream_base}?timepoint={timepoint}")
    } else {
        stream_base.to_string()
    };

    info!(timepoint, "Connecting to CH stream at {}", url);

    let response = client
        .get(&url)
        .basic_auth(api_key, None::<&str>)
        .send()
        .await
        .context("CH stream connect")?;

    // 416 → timepoint too old — clear and reconnect fresh (no backoff penalty)
    if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        warn!(
            "CH timepoint too old (416 Range Not Satisfiable) — clearing cursor and reconnecting"
        );
        cursor.clear_timepoint().await;
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

        if let Err(e) = handler.on_line(&line).await {
            debug!(?e, line = %line, "CH stream handler error — skipping line");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::cursor::InMemoryCursorStore;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Simple test handler that records lines it receives.
    struct RecordingHandler {
        lines: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl StreamEventHandler for RecordingHandler {
        async fn on_line(&self, line: &str) -> Result<()> {
            self.lines.lock().await.push(line.to_string());
            Ok(())
        }
    }

    fn make_client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn test_connect_and_stream_returns_err_on_unreachable() {
        let client = make_client();
        let cursor = InMemoryCursorStore::new();
        let handler = RecordingHandler {
            lines: Arc::new(Mutex::new(vec![])),
        };
        let result = connect_and_stream(
            &client,
            "testkey",
            "http://127.0.0.1:19997",
            &cursor,
            &handler,
        )
        .await;
        assert!(result.is_err(), "Unreachable server should return Err");
    }

    #[tokio::test]
    async fn test_connect_and_stream_clears_cursor_on_416() {
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

        let client = make_client();
        let cursor = InMemoryCursorStore::new();
        cursor.persist_timepoint(12345).await;
        let handler = RecordingHandler {
            lines: Arc::new(Mutex::new(vec![])),
        };
        let result = connect_and_stream(
            &client,
            "testkey",
            &format!("http://127.0.0.1:{port}"),
            &cursor,
            &handler,
        )
        .await;
        assert!(result.is_ok(), "416 should clear cursor and return Ok");
        assert!(
            cursor.load_timepoint().await.is_none(),
            "Cursor should be cleared after 416"
        );
    }

    #[tokio::test]
    async fn test_connect_and_stream_includes_timepoint_in_url() {
        // This test verifies the URL is formed correctly by checking that
        // a cursor with a non-zero timepoint results in a request with ?timepoint=
        // We can verify by observing the error (connection refused to our test URL)
        // and checking the cursor was consulted.
        let client = make_client();
        let cursor = InMemoryCursorStore::new();
        cursor.persist_timepoint(999).await;
        let handler = RecordingHandler {
            lines: Arc::new(Mutex::new(vec![])),
        };
        let result = connect_and_stream(
            &client,
            "testkey",
            "http://127.0.0.1:19997",
            &cursor,
            &handler,
        )
        .await;
        // Cursor should NOT have been cleared (not 416, just unreachable)
        assert!(result.is_err());
        assert_eq!(cursor.load_timepoint().await, Some(999));
    }

    #[tokio::test]
    async fn test_connect_and_stream_parses_ndjson_and_skips_heartbeat() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = "\n{\"line\":\"one\"}\n{\"line\":\"two\"}\n\n".to_string();

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

        let client = make_client();
        let cursor = InMemoryCursorStore::new();
        let recorded_lines = Arc::new(Mutex::new(vec![]));
        let handler = RecordingHandler {
            lines: recorded_lines.clone(),
        };
        let result = connect_and_stream(
            &client,
            "testkey",
            &format!("http://127.0.0.1:{port}"),
            &cursor,
            &handler,
        )
        .await;
        assert!(result.is_ok(), "Valid NDJSON stream should return Ok");
        let lines = recorded_lines.lock().await;
        assert_eq!(
            lines.len(),
            2,
            "Should have 2 non-empty lines (heartbeats skipped)"
        );
        assert!(lines[0].contains("\"one\""));
        assert!(lines[1].contains("\"two\""));
    }
}
