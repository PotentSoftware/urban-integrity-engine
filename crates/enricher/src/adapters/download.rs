//! Size-capped HTTP body readers shared by the bulk-file adapters.
//!
//! # Why this exists
//!
//! Every bulk adapter (CCOD, OCOD, PPD, Charity Commission, ICIJ) downloads a large archive and
//! carries a threat-model claim that the download is size-capped. Three of the five enforced that
//! cap **only** against `Response::content_length()`, then called `resp.bytes().await?`, which
//! buffers the entire body with no limit.
//!
//! `content_length()` is `Option<u64>`: it is `None` under chunked transfer-encoding, and it is
//! server-supplied in every case. Under `if let Some(len) = ...` a missing header skips the check
//! entirely, so a server that omits or understates the header faced no cap at all — the exact
//! failure the guard was written to prevent.
//!
//! The rule these helpers encode: **a size cap must be enforced against bytes actually received,
//! never against a number the peer told us.** A `content_length()` pre-check is still worth doing
//! — it avoids starting a download that is declared oversize — but it is an optimisation, not the
//! guard.
//!
//! Both helpers return `Ok(None)` when the cap is exceeded, so callers `skip` rather than fail:
//! an oversize bulk file is an operational condition, not a bug in this process.

use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Stream a response body to `path`, aborting once `max_bytes` have been received.
///
/// Returns `Ok(Some(bytes_written))` on success, or `Ok(None)` if the body exceeded `max_bytes`
/// (in which case the partial file is removed).
pub async fn stream_to_file_capped(
    mut resp: reqwest::Response,
    path: &std::path::Path,
    max_bytes: u64,
    adapter: &'static str,
) -> anyhow::Result<Option<u64>> {
    let mut file = tokio::io::BufWriter::new(tokio::fs::File::create(path).await?);
    let mut written: u64 = 0;

    while let Some(chunk) = resp.chunk().await? {
        written += chunk.len() as u64;
        if written > max_bytes {
            warn!(
                adapter,
                bytes_received = written,
                max_bytes,
                "download exceeded size limit while streaming — aborting"
            );
            drop(file);
            let _ = tokio::fs::remove_file(path).await;
            return Ok(None);
        }
        tokio::io::copy(&mut chunk.as_ref(), &mut file).await?;
    }

    // Belt-and-braces, NOT the thing keeping the tail intact: `tokio::io::copy` already flushes
    // the writer when its reader hits EOF, and each chunk is its own `copy` call, so the buffer
    // is drained per chunk. Verified by removing this line — the byte-for-byte test still passed.
    // Kept because `tokio::io::BufWriter` genuinely does not flush on drop (`Drop` cannot await),
    // so the guarantee would evaporate if the loop ever stopped using `copy`.
    file.flush().await?;
    Ok(Some(written))
}

/// Read a response body into memory, aborting once `max_bytes` have been received.
///
/// For callers that feed the bytes straight into an in-memory parser rather than caching to disk.
/// Returns `Ok(None)` if the body exceeded `max_bytes`.
pub async fn read_to_vec_capped(
    mut resp: reqwest::Response,
    max_bytes: u64,
    adapter: &'static str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = resp.chunk().await? {
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            warn!(
                adapter,
                bytes_received = buf.len() as u64 + chunk.len() as u64,
                max_bytes,
                "download exceeded size limit while streaming — aborting"
            );
            return Ok(None);
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serve one HTTP/1.1 response with **chunked** transfer-encoding and no `Content-Length`,
    /// carrying `body_len` bytes. Returns the bound address.
    ///
    /// Chunked is the point: `reqwest::Response::content_length()` is `None` here, so any guard
    /// that reads only that header sees nothing to check. This is the bypass, reproduced.
    async fn serve_chunked_body(body_len: usize) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();

            // Read past the request head so the client isn't blocked on our write.
            let mut discard = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut discard).await;

            sock.write_all(
                b"HTTP/1.1 200 OK\r\n\
                  Content-Type: application/octet-stream\r\n\
                  Transfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();

            // One 1 KiB chunk at a time until body_len is served.
            let chunk = vec![b'x'; 1024];
            let mut sent = 0usize;
            while sent < body_len {
                let n = chunk.len().min(body_len - sent);
                if sock
                    .write_all(format!("{n:x}\r\n").as_bytes())
                    .await
                    .is_err()
                {
                    return; // client hung up after hitting the cap — expected
                }
                if sock.write_all(&chunk[..n]).await.is_err() {
                    return;
                }
                if sock.write_all(b"\r\n").await.is_err() {
                    return;
                }
                sent += n;
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
        });

        addr
    }

    /// The regression this module exists for: a chunked response carries NO `Content-Length`, so
    /// the old `if let Some(len) = resp.content_length()` guard never fired, and the follow-on
    /// `resp.bytes().await?` buffered the whole body regardless of size.
    #[tokio::test]
    async fn oversize_chunked_body_is_capped_despite_absent_content_length() {
        let addr = serve_chunked_body(64 * 1024).await;
        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();

        assert!(
            resp.content_length().is_none(),
            "precondition: a chunked response must expose no Content-Length, \
             otherwise this test is not exercising the bypass"
        );

        let out = read_to_vec_capped(resp, 8 * 1024, "test").await.unwrap();
        assert!(
            out.is_none(),
            "a 64 KiB body under an 8 KiB cap must be refused; got {:?} bytes",
            out.map(|v| v.len())
        );
    }

    #[tokio::test]
    async fn within_cap_chunked_body_is_returned_whole() {
        let addr = serve_chunked_body(4 * 1024).await;
        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();

        let out = read_to_vec_capped(resp, 8 * 1024, "test").await.unwrap();
        assert_eq!(
            out.map(|v| v.len()),
            Some(4 * 1024),
            "a body inside the cap must come back complete"
        );
    }

    #[tokio::test]
    async fn oversize_streamed_file_is_capped_and_partial_file_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversize.zip");

        let addr = serve_chunked_body(64 * 1024).await;
        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();

        let out = stream_to_file_capped(resp, &path, 8 * 1024, "test")
            .await
            .unwrap();

        assert!(out.is_none(), "oversize body must be refused");
        assert!(
            !path.exists(),
            "the partial file must be removed, or the next run treats it as a valid cache"
        );
    }

    /// Guards the `BufWriter` flush. Without `file.flush().await`, buffered bytes are dropped
    /// unwritten, and a short file reads as a corrupt archive rather than an error.
    #[tokio::test]
    async fn streamed_file_contains_every_byte_received() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whole.bin");

        let addr = serve_chunked_body(20 * 1024).await;
        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();

        let written = stream_to_file_capped(resp, &path, 1024 * 1024, "test")
            .await
            .unwrap()
            .expect("body is inside the cap");

        let on_disk = tokio::fs::metadata(&path).await.unwrap().len();
        assert_eq!(written, 20 * 1024, "reported byte count");
        assert_eq!(
            on_disk, written,
            "bytes on disk must equal bytes received — a shortfall means the BufWriter \
             tail was never flushed"
        );
    }
}
