use tracing_subscriber::{fmt, EnvFilter};

/// Initialise the global tracing subscriber.
///
/// - Uses `RUST_LOG` for level filtering (default: `info`).
/// - Outputs JSON when `RUST_LOG_FORMAT=json` is set (production/serve);
///   otherwise uses human-readable format (CLI commands, development).
/// - Always writes to **stderr** so that structured stdout (JSON report/alert
///   output) remains parseable even when warn/info messages are emitted.
/// - Uses `try_init()` so calling this more than once (e.g. in tests) is a
///   no-op instead of a panic.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if std::env::var("RUST_LOG_FORMAT").as_deref() == Ok("json") {
        let _ = fmt()
            .with_env_filter(filter)
            .json()
            .with_writer(std::io::stderr)
            .try_init();
    } else {
        let _ = fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    }
}
